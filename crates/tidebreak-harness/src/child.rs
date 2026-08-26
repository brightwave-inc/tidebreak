//! Bookkeeping for adapters that own an engine child.
//!
//! Two facts the session worker needs, and a pid read at turn boundaries
//! cannot give it: the pid *while* the turn is in flight (that is the whole
//! window a crash can orphan a child in), and how the child ended once it is
//! gone (an EOF on stdout is not a completed turn).

use std::collections::HashMap;
use std::io;
use std::process::{ExitStatus, Output};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::watch;
#[cfg(unix)]
use tokio::time::{sleep, Instant};

use crate::TurnOutcome;

static SPAWNED_PROCESS_IDENTITIES: LazyLock<Mutex<HashMap<i64, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Result of terminating and waiting for one recorded process identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordedProcessReap {
    /// The recorded process exited, or the pid now belongs to another process.
    Exited,
    /// The recorded process still had the same identity when the wait expired.
    TimedOut,
}

/// Which end of one subprocess stream survives an output limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputRetention {
    /// Keep the first bytes and lines that the process wrote.
    Head,
    /// Keep the newest bytes and lines that the process wrote.
    Tail,
}

/// Memory limits for one piped subprocess stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputBudget {
    /// Maximum retained bytes.
    pub max_bytes: usize,
    /// Maximum retained newline-delimited records.
    pub max_lines: usize,
    /// Which end survives when either limit is exceeded.
    pub retention: OutputRetention,
}

impl OutputBudget {
    /// Create a budget that keeps the stream's leading bytes and lines.
    #[must_use]
    pub const fn head(max_bytes: usize, max_lines: usize) -> Self {
        Self {
            max_bytes,
            max_lines,
            retention: OutputRetention::Head,
        }
    }

    /// Create a budget that keeps the stream's trailing bytes and lines.
    #[must_use]
    pub const fn tail(max_bytes: usize, max_lines: usize) -> Self {
        Self {
            max_bytes,
            max_lines,
            retention: OutputRetention::Tail,
        }
    }
}

/// One subprocess stream captured within its byte and line limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedOutput {
    /// Retained bytes from the selected end of the stream.
    pub bytes: Vec<u8>,
    /// Whether bytes or lines were discarded.
    pub truncated: bool,
    /// Which end of the stream was retained.
    pub retention: OutputRetention,
}

impl BoundedOutput {
    /// Decode the retained bytes and insert an explicit truncation marker at
    /// the discarded end.
    #[must_use]
    pub fn into_marked_text(self) -> String {
        let text = String::from_utf8_lossy(&self.bytes).into_owned();
        if !self.truncated {
            return text;
        }
        match self.retention {
            OutputRetention::Head => {
                if text.ends_with('\n') {
                    format!("{text}[output truncated]\n")
                } else {
                    format!("{text}\n[output truncated]")
                }
            }
            OutputRetention::Tail => {
                if text.starts_with('\n') {
                    format!("[output truncated]{text}")
                } else {
                    format!("[output truncated]\n{text}")
                }
            }
        }
    }
}

/// Exit status and bounded stdout and stderr from one process tree.
#[derive(Debug)]
pub struct BoundedProcessOutput {
    /// Root process exit status.
    pub status: ExitStatus,
    /// Bounded stdout.
    pub stdout: BoundedOutput,
    /// Bounded stderr.
    pub stderr: BoundedOutput,
    /// Whether the process tree was terminated after either stream exceeded
    /// its budget.
    pub terminated_for_output: bool,
}

/// Return the creation identity captured when this process spawned `pid`.
///
/// The registry is process-local and only covers children created through
/// [`spawn_process_tree`]. Recovery after a host restart uses the durable copy
/// stored beside the pid instead.
#[must_use]
pub fn spawned_process_identity(pid: i64) -> Option<String> {
    SPAWNED_PROCESS_IDENTITIES
        .lock()
        .expect("spawned process identities")
        .get(&pid)
        .cloned()
}

/// Read the operating system's non-reusable creation identity for `pid`.
///
/// `Ok(None)` means that no process owns the pid. An error is ambiguous and
/// must keep a recovery fence in place.
pub fn current_process_identity(pid: i64) -> io::Result<Option<String>> {
    if pid <= 0 {
        return Ok(None);
    }
    platform_process_identity(pid)
}

/// Terminate the process only when `pid` still has `expected_identity`, then
/// wait until that exact identity no longer exists.
///
/// A reused pid is treated as proof that the recorded process exited. The
/// replacement process is never signaled.
pub async fn terminate_recorded_process(
    pid: i64,
    expected_identity: &str,
    timeout: Duration,
) -> io::Result<RecordedProcessReap> {
    if expected_identity.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "recorded process identity is empty",
        ));
    }
    terminate_recorded_process_platform(pid, expected_identity, timeout).await
}

fn register_spawned_process(pid: u32) -> io::Result<String> {
    let pid = i64::from(pid);
    let identity = current_process_identity(pid)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "spawned child exited before its process identity was recorded",
        )
    })?;
    let mut identities = SPAWNED_PROCESS_IDENTITIES
        .lock()
        .expect("spawned process identities");
    identities.remove(&pid);
    identities.insert(pid, identity.clone());
    Ok(identity)
}

fn unregister_spawned_process(pid: u32, identity: &str) {
    let pid = i64::from(pid);
    let mut identities = SPAWNED_PROCESS_IDENTITIES
        .lock()
        .expect("spawned process identities");
    if identities
        .get(&pid)
        .is_some_and(|stored| stored == identity)
    {
        identities.remove(&pid);
    }
}

/// Live pid of the child backing the current turn.
///
/// Every transition is published, so a watcher can persist the pid the moment
/// the child exists rather than discovering it after the turn is over.
#[derive(Debug)]
pub struct ChildPid {
    tx: watch::Sender<Option<i64>>,
}

impl Default for ChildPid {
    fn default() -> Self {
        Self::new()
    }
}

impl ChildPid {
    /// A cell with no child recorded.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tx: watch::Sender::new(None),
        }
    }

    /// Publish the pid of a child that now exists. An absent or zero pid is
    /// published as "no child": recovery must never probe an invented pid.
    pub fn set(&self, pid: Option<u32>) {
        let pid = pid.filter(|pid| *pid != 0).map(i64::from);
        self.tx.send_replace(pid);
    }

    /// Publish that no child is running.
    pub fn clear(&self) {
        self.tx.send_replace(None);
    }

    /// The pid recorded right now, if any.
    #[must_use]
    pub fn get(&self) -> Option<i64> {
        *self.tx.borrow()
    }

    /// Watch every transition, starting from the current value.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Option<i64>> {
        self.tx.subscribe()
    }
}

/// One child and the operating-system containment boundary it owns.
///
/// Windows children are created suspended, assigned to a Job Object configured
/// with `KILL_ON_JOB_CLOSE`, then resumed. That closes the usual spawn/assign
/// race where a wrapper such as `npm.cmd` could create a descendant before it
/// became part of the job. Dropping this value terminates the job tree.
///
/// Unix children lead a dedicated process group. Ordinary wrappers and their
/// inherited descendants remain in that group; a descendant that deliberately
/// calls `setsid` or changes groups is outside this boundary. [`Self::interrupt`]
/// sends SIGINT to the group first and escalates after the caller's grace
/// period; dropping an unreaped value kills the group.
pub struct ProcessTreeChild {
    child: Option<Child>,
    process_id: u32,
    process_identity: String,
    #[cfg(unix)]
    process_group: Option<libc::pid_t>,
    #[cfg(windows)]
    job: windows::Job,
}

/// Spawn `command` with platform process containment.
///
/// Callers may await [`ProcessTreeChild::wait_with_output`] inside a timeout:
/// cancelling that future drops the owned child and terminates its containment
/// boundary.
pub fn spawn_process_tree(command: &mut Command) -> io::Result<ProcessTreeChild> {
    command.kill_on_drop(true);
    #[cfg(windows)]
    {
        let job = windows::Job::new()?;
        command.creation_flags(windows::CREATE_SUSPENDED_FLAG);
        let mut child = command.spawn()?;
        if let Err(err) = job.assign_and_resume(&child) {
            let _ = child.start_kill();
            return Err(err);
        }
        let process_id = child
            .id()
            .ok_or_else(|| io::Error::other("spawned child has no process id"))?;
        let process_identity = match register_spawned_process(process_id) {
            Ok(identity) => identity,
            Err(error) => {
                let _ = job.terminate();
                let _ = child.start_kill();
                return Err(error);
            }
        };
        return Ok(ProcessTreeChild {
            child: Some(child),
            process_id,
            process_identity,
            job,
        });
    }
    #[cfg(unix)]
    {
        // Tokio delegates this safe setup to `std::process::Command`, which
        // calls setpgid in the child before exec. There is no post-spawn window
        // where a wrapper can create descendants outside the owned group.
        command.process_group(0);
        let mut child = command.spawn()?;
        let process_id = match child.id() {
            Some(process_id) => process_id,
            None => {
                let _ = child.start_kill();
                return Err(io::Error::other("spawned child has no valid process id"));
            }
        };
        let process_group = match libc::pid_t::try_from(process_id) {
            Ok(process_group) => process_group,
            Err(_) => {
                let _ = child.start_kill();
                return Err(io::Error::other("spawned child has no valid process id"));
            }
        };
        let process_identity = match register_spawned_process(process_id) {
            Ok(identity) => identity,
            Err(error) => {
                let _ = kill_process_group(process_group);
                let _ = child.start_kill();
                return Err(error);
            }
        };
        Ok(ProcessTreeChild {
            child: Some(child),
            process_id,
            process_identity,
            process_group: Some(process_group),
        })
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        let mut child = command.spawn()?;
        let process_id = child
            .id()
            .ok_or_else(|| io::Error::other("spawned child has no process id"))?;
        let process_identity = match register_spawned_process(process_id) {
            Ok(identity) => identity,
            Err(error) => {
                let _ = child.start_kill();
                return Err(error);
            }
        };
        Ok(ProcessTreeChild {
            child: Some(child),
            process_id,
            process_identity,
        })
    }
}

impl ProcessTreeChild {
    fn child(&self) -> &Child {
        self.child.as_ref().expect("process child is present")
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("process child is present")
    }

    /// Operating-system process id, when the child is still present.
    #[must_use]
    pub fn id(&self) -> Option<u32> {
        self.child().id()
    }

    /// Non-reusable creation identity captured before the child was exposed.
    #[must_use]
    pub fn process_identity(&self) -> &str {
        &self.process_identity
    }

    /// Take the child's piped stdin, when configured.
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child_mut().stdin.take()
    }

    /// Take the child's piped stdout, when configured.
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child_mut().stdout.take()
    }

    /// Take the child's piped stderr, when configured.
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child_mut().stderr.take()
    }

    /// Wait for the root child to exit, ending Unix group ownership.
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.child_mut().wait().await;
        if status.is_ok() {
            unregister_spawned_process(self.process_id, &self.process_identity);
        }
        #[cfg(unix)]
        if status.is_ok() {
            // Once the leader is reaped its numeric pid/pgid may be reused.
            // Never retain that bare id for a later Drop or signal.
            self.process_group = None;
        }
        status
    }

    /// Read a completed root child's exit status, ending Unix group ownership.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.child_mut().try_wait();
        if matches!(status, Ok(Some(_))) {
            unregister_spawned_process(self.process_id, &self.process_identity);
        }
        #[cfg(unix)]
        if matches!(status, Ok(Some(_))) {
            // See `wait`: a reaped leader no longer pins the numeric pgid.
            self.process_group = None;
        }
        status
    }

    /// Drain inherited stdout/stderr, then reap the root child.
    ///
    /// Dropping this future before completion drops `self`, which terminates
    /// the owned containment boundary.
    pub async fn wait_with_output(mut self) -> io::Result<Output> {
        drop(self.take_stdin());
        let stdout = self.take_stdout();
        let stderr = self.take_stderr();
        let read_stdout = async move {
            let mut bytes = Vec::new();
            if let Some(mut stdout) = stdout {
                stdout.read_to_end(&mut bytes).await?;
            }
            Ok::<_, io::Error>(bytes)
        };
        let read_stderr = async move {
            let mut bytes = Vec::new();
            if let Some(mut stderr) = stderr {
                stderr.read_to_end(&mut bytes).await?;
            }
            Ok::<_, io::Error>(bytes)
        };
        let (stdout, stderr) = tokio::try_join!(read_stdout, read_stderr)?;
        let status = self.wait().await?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }

    /// Capture stdout and stderr concurrently without exceeding either
    /// stream's byte or line budget.
    ///
    /// If `terminate_on_limit` is true, the first discarded byte terminates
    /// the owned process tree. The returned buffers still contain the bounded
    /// head or tail selected by each budget. Cancelling this future keeps the
    /// ordinary drop behavior and terminates the tree.
    pub async fn wait_with_bounded_output(
        mut self,
        stdout_budget: OutputBudget,
        stderr_budget: OutputBudget,
        terminate_on_limit: bool,
    ) -> io::Result<BoundedProcessOutput> {
        drop(self.take_stdin());
        let stdout = self.take_stdout();
        let stderr = self.take_stderr();
        let (limit_tx, mut limit_rx) = watch::channel(false);
        let read_stdout = capture_bounded(stdout, stdout_budget, limit_tx.clone());
        let read_stderr = capture_bounded(stderr, stderr_budget, limit_tx);
        let readers = async move { tokio::try_join!(read_stdout, read_stderr) };
        tokio::pin!(readers);

        let mut terminated_for_output = false;
        let mut limit_open = true;
        let (stdout, stderr) = loop {
            tokio::select! {
                result = &mut readers => break result?,
                changed = limit_rx.changed(), if terminate_on_limit && !terminated_for_output && limit_open => {
                    if changed.is_ok() && *limit_rx.borrow_and_update() {
                        self.terminate_tree()?;
                        terminated_for_output = true;
                    } else if changed.is_err() {
                        limit_open = false;
                    }
                }
            }
        };
        let status = self.wait().await?;
        Ok(BoundedProcessOutput {
            status,
            stdout,
            stderr,
            terminated_for_output,
        })
    }

    /// Ask the process to stop, escalating to tree termination after `grace`.
    ///
    /// Windows has no reliable console-control channel for GUI-spawned batch
    /// shims, so interruption terminates the Job Object immediately. Unix
    /// sends SIGINT to the owned process group, then sends SIGKILL after the
    /// grace period before reaping the group leader. Keeping the leader
    /// unreaped pins the numeric process-group id so escalation cannot target
    /// an unrelated group after pid reuse. Cancelling this future also sends
    /// SIGKILL through a drop guard.
    pub async fn interrupt(&mut self, grace: Duration) -> io::Result<ExitStatus> {
        #[cfg(unix)]
        {
            let Some(process_group) = self.process_group.take() else {
                return self.wait().await;
            };
            let mut guard = ProcessGroupGuard::new(Some(process_group));
            guard.signal(libc::SIGINT)?;
            sleep(grace).await;
            guard.kill()?;
            let status = self.child_mut().wait().await;
            if status.is_ok() {
                unregister_spawned_process(self.process_id, &self.process_identity);
                guard.disarm();
            }
            status
        }
        #[cfg(not(unix))]
        {
            let _ = grace;
            self.terminate().await
        }
    }

    /// Terminate the entire owned tree and wait for the root child.
    pub async fn terminate(&mut self) -> io::Result<ExitStatus> {
        #[cfg(unix)]
        {
            let mut guard = ProcessGroupGuard::new(self.process_group.take());
            guard.kill()?;
            let status = self.child_mut().wait().await;
            if status.is_ok() {
                unregister_spawned_process(self.process_id, &self.process_identity);
                guard.disarm();
            }
            status
        }
        #[cfg(not(unix))]
        {
            self.terminate_tree()?;
            self.wait().await
        }
    }

    fn terminate_tree(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        {
            self.job.terminate()
        }
        #[cfg(unix)]
        {
            let Some(process_group) = self.process_group else {
                return Ok(());
            };
            kill_process_group(process_group)?;
            self.process_group = None;
            Ok(())
        }
        #[cfg(all(not(unix), not(windows)))]
        {
            self.child_mut().start_kill()
        }
    }
}

const BOUNDED_OUTPUT_CHUNK_BYTES: usize = 8 * 1_024;

async fn capture_bounded<R>(
    reader: Option<R>,
    budget: OutputBudget,
    limit_tx: watch::Sender<bool>,
) -> io::Result<BoundedOutput>
where
    R: AsyncRead + Unpin,
{
    let Some(mut reader) = reader else {
        return Ok(BoundedOutput {
            bytes: Vec::new(),
            truncated: false,
            retention: budget.retention,
        });
    };
    let mut capture = OutputCapture::new(budget);
    // Keep the read buffer off the async future's stack. Callers can nest this
    // future through several git helpers, and two inline 8 KiB buffers per
    // process can overflow the small stack used by Rust's test threads.
    let mut chunk = vec![0_u8; BOUNDED_OUTPUT_CHUNK_BYTES];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        if capture.push(&chunk[..read]) {
            limit_tx.send_replace(true);
        }
    }
    Ok(capture.finish())
}

#[derive(Debug)]
struct OutputCapture {
    budget: OutputBudget,
    bytes: Vec<u8>,
    newline_count: usize,
    truncated: bool,
}

impl OutputCapture {
    fn new(budget: OutputBudget) -> Self {
        Self {
            budget,
            bytes: Vec::with_capacity(budget.max_bytes.min(BOUNDED_OUTPUT_CHUNK_BYTES)),
            newline_count: 0,
            truncated: false,
        }
    }

    /// Returns true only when this push crosses a limit for the first time.
    fn push(&mut self, chunk: &[u8]) -> bool {
        let was_truncated = self.truncated;
        match self.budget.retention {
            OutputRetention::Head => self.push_head(chunk),
            OutputRetention::Tail => self.push_tail(chunk),
        }
        !was_truncated && self.truncated
    }

    fn push_head(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        let byte_room = self.budget.max_bytes.saturating_sub(self.bytes.len());
        let mut take = chunk.len().min(byte_room);
        if self.newline_count >= self.budget.max_lines {
            take = 0;
        } else if self.budget.max_lines != usize::MAX {
            let remaining_lines = self.budget.max_lines - self.newline_count;
            if let Some(index) = nth_newline(chunk, remaining_lines) {
                take = take.min(index.saturating_add(1));
            }
        }
        let kept = &chunk[..take];
        self.newline_count = self
            .newline_count
            .saturating_add(kept.iter().filter(|byte| **byte == b'\n').count());
        self.bytes.extend_from_slice(kept);
        self.truncated |= take < chunk.len();
    }

    fn push_tail(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        if self.budget.max_bytes == 0 || self.budget.max_lines == 0 {
            self.truncated = true;
            return;
        }
        if chunk.len() >= self.budget.max_bytes {
            let dropped = !self.bytes.is_empty() || chunk.len() > self.budget.max_bytes;
            self.bytes.clear();
            self.bytes
                .extend_from_slice(&chunk[chunk.len() - self.budget.max_bytes..]);
            self.truncated |= dropped;
        } else {
            let overflow = self
                .bytes
                .len()
                .saturating_add(chunk.len())
                .saturating_sub(self.budget.max_bytes);
            if overflow > 0 {
                self.bytes.drain(..overflow);
                self.truncated = true;
            }
            self.bytes.extend_from_slice(chunk);
        }
        self.newline_count = self.bytes.iter().filter(|byte| **byte == b'\n').count();
        if self.budget.max_lines != usize::MAX {
            while retained_line_count(&self.bytes, self.newline_count) > self.budget.max_lines {
                let Some(end) = self.bytes.iter().position(|byte| *byte == b'\n') else {
                    self.bytes.clear();
                    self.newline_count = 0;
                    self.truncated = true;
                    break;
                };
                self.bytes.drain(..=end);
                self.newline_count -= 1;
                self.truncated = true;
            }
        }
    }

    fn finish(self) -> BoundedOutput {
        BoundedOutput {
            bytes: self.bytes,
            truncated: self.truncated,
            retention: self.budget.retention,
        }
    }
}

fn retained_line_count(bytes: &[u8], newline_count: usize) -> usize {
    newline_count.saturating_add(usize::from(
        !bytes.is_empty() && bytes.last() != Some(&b'\n'),
    ))
}

fn nth_newline(bytes: &[u8], count: usize) -> Option<usize> {
    if count == 0 {
        return Some(0);
    }
    let mut seen = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            seen += 1;
            if seen == count {
                return Some(index);
            }
        }
    }
    None
}

impl Drop for ProcessTreeChild {
    fn drop(&mut self) {
        #[cfg(unix)]
        if self.process_group.is_some() {
            let _ = self.terminate_tree();
        }
        #[cfg(not(unix))]
        if self.child.is_some() {
            let _ = self.terminate_tree();
        }
        unregister_spawned_process(self.process_id, &self.process_identity);
    }
}

#[cfg(target_os = "linux")]
fn platform_process_identity(pid: i64) -> io::Result<Option<String>> {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let Some(command_end) = stat.rfind(')') else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "process stat has no command terminator",
        ));
    };
    let start_ticks = stat[command_end + 1..]
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "process stat is truncated"))?
        .parse::<u64>()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    Ok(Some(format!("linux:{}:{start_ticks}", boot_id.trim())))
}

#[cfg(target_os = "macos")]
fn platform_process_identity(pid: i64) -> io::Result<Option<String>> {
    let raw_pid = libc::pid_t::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid is out of range"))?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>())
        .expect("proc_bsdinfo fits in c_int");
    // SAFETY: `info` points to `size` writable bytes, and the pid is only
    // queried. `proc_pidinfo` initializes the full structure on success.
    let read = unsafe {
        libc::proc_pidinfo(
            raw_pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read == size {
        // SAFETY: the full structure was initialized when `read == size`.
        let info = unsafe { info.assume_init() };
        return Ok(Some(format!(
            "macos:{}:{}",
            info.pbi_start_tvsec, info.pbi_start_tvusec
        )));
    }
    // SAFETY: signal 0 only checks whether the pid exists and is signalable.
    if unsafe { libc::kill(raw_pid, 0) } != 0
        && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    {
        return Ok(None);
    }
    Err(io::Error::last_os_error())
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn platform_process_identity(_pid: i64) -> io::Result<Option<String>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process creation identity is unavailable on this platform",
    ))
}

#[cfg(windows)]
fn platform_process_identity(pid: i64) -> io::Result<Option<String>> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::{ERROR_INVALID_PARAMETER, HANDLE};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    let pid = u32::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid is out of range"))?;
    // SAFETY: this opens a query-only handle for a numeric pid.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        let error = io::Error::last_os_error();
        if error.raw_os_error().map(|code| code as u32) == Some(ERROR_INVALID_PARAMETER) {
            return Ok(None);
        }
        return Err(error);
    }
    // SAFETY: successful `OpenProcess` returns one owned handle.
    let process = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    windows_process_identity(process.as_raw_handle() as HANDLE).map(Some)
}

#[cfg(windows)]
fn windows_process_identity(process: windows_sys::Win32::Foundation::HANDLE) -> io::Result<String> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetProcessTimes;

    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: `process` is a live process handle and every FILETIME pointer is
    // writable for the duration of the call.
    if unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let ticks = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
    Ok(format!("windows:{ticks}"))
}

#[cfg(not(any(unix, windows)))]
fn platform_process_identity(_pid: i64) -> io::Result<Option<String>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process creation identity is unavailable on this platform",
    ))
}

#[cfg(unix)]
async fn terminate_recorded_process_platform(
    pid: i64,
    expected_identity: &str,
    timeout: Duration,
) -> io::Result<RecordedProcessReap> {
    match current_process_identity(pid)? {
        None => return Ok(RecordedProcessReap::Exited),
        Some(observed) if observed != expected_identity => {
            return Ok(RecordedProcessReap::Exited);
        }
        Some(_) => {}
    }
    let process_group = libc::pid_t::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid is out of range"))?;
    kill_process_group(process_group)?;
    let deadline = Instant::now() + timeout;
    loop {
        match current_process_identity(pid)? {
            None => return Ok(RecordedProcessReap::Exited),
            Some(observed) if observed != expected_identity => {
                return Ok(RecordedProcessReap::Exited);
            }
            Some(_) => {}
        }
        if Instant::now() >= deadline {
            return Ok(RecordedProcessReap::TimedOut);
        }
        sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(windows)]
async fn terminate_recorded_process_platform(
    pid: i64,
    expected_identity: &str,
    timeout: Duration,
) -> io::Result<RecordedProcessReap> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::{
        ERROR_INVALID_PARAMETER, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    let pid = u32::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid is out of range"))?;
    // SAFETY: the requested rights are limited to identity query, termination,
    // and waiting for the exact process object behind this pid.
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if raw.is_null() {
        let error = io::Error::last_os_error();
        if error.raw_os_error().map(|code| code as u32) == Some(ERROR_INVALID_PARAMETER) {
            return Ok(RecordedProcessReap::Exited);
        }
        return Err(error);
    }
    // SAFETY: successful `OpenProcess` returns one owned handle.
    let process = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    let handle = process.as_raw_handle() as HANDLE;
    if windows_process_identity(handle)? != expected_identity {
        return Ok(RecordedProcessReap::Exited);
    }
    // SAFETY: the handle carries PROCESS_TERMINATE for this exact process.
    if unsafe { TerminateProcess(handle, 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let millis = timeout.as_millis().min(u128::from(u32::MAX)) as u32;
    // SAFETY: the handle carries synchronization rights and stays owned for
    // the whole bounded wait.
    match unsafe { WaitForSingleObject(handle, millis) } {
        WAIT_OBJECT_0 => Ok(RecordedProcessReap::Exited),
        WAIT_TIMEOUT => Ok(RecordedProcessReap::TimedOut),
        _ => Err(io::Error::last_os_error()),
    }
}

#[cfg(not(any(unix, windows)))]
async fn terminate_recorded_process_platform(
    _pid: i64,
    _expected_identity: &str,
    _timeout: Duration,
) -> io::Result<RecordedProcessReap> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process termination is unavailable on this platform",
    ))
}

/// How many times a group kill re-walks the group before giving up.
#[cfg(unix)]
const GROUP_KILL_ROUNDS: usize = 3;

/// How long a fork that was in flight during a walk gets to become visible to
/// the next one.
#[cfg(unix)]
const GROUP_KILL_SETTLE: Duration = Duration::from_millis(1);

/// Kill an owned process group, then keep killing until nothing answers.
///
/// `kill(2)` walks the group once. A descendant that forks while that walk is
/// in flight never receives the signal, and when its parent dies it is
/// reparented to init — still in the group, still holding the stdout the
/// caller is waiting on for EOF. A single group kill therefore leaks a live
/// process and a pipe that never closes, which is exactly the hang this type
/// exists to prevent.
///
/// Stopping the group first closes most of that window, because a stopped
/// member cannot start another fork. Re-walking closes the rest: the one fork
/// already in flight when the stop landed is an ordinary group member by the
/// next round. The stop is best effort — only the kill decides the result.
///
/// This blocks for at most a couple of milliseconds, because it also runs from
/// `Drop`, where there is no runtime to await on.
#[cfg(unix)]
fn kill_process_group(process_group: libc::pid_t) -> io::Result<()> {
    for round in 0..GROUP_KILL_ROUNDS {
        if round > 0 {
            std::thread::sleep(GROUP_KILL_SETTLE);
        }
        let _ = signal_process_group(process_group, libc::SIGSTOP);
        signal_process_group(process_group, libc::SIGKILL)?;
        if process_group_is_gone(process_group) {
            break;
        }
    }
    Ok(())
}

/// Whether `kill(2)` can still find the group.
///
/// A leader kept unreaped to pin the group id keeps answering, so this is an
/// early exit rather than a decision; [`kill_process_group`] bounds the rounds.
#[cfg(unix)]
fn process_group_is_gone(process_group: libc::pid_t) -> bool {
    // SAFETY: signal 0 only probes whether the group can be signalled.
    if unsafe { libc::kill(-process_group, 0) } == 0 {
        return false;
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => true,
        // See `signal_process_group`: macOS reports EPERM for a group holding
        // only zombies.
        Some(libc::EPERM) => cfg!(target_os = "macos"),
        _ => false,
    }
}

#[cfg(unix)]
fn signal_process_group(process_group: libc::pid_t, signal: libc::c_int) -> io::Result<()> {
    debug_assert!(process_group > 0);
    // SAFETY: a negative pid asks kill(2) to signal the process group whose
    // positive id was captured from the child created with process_group(0).
    if unsafe { libc::kill(-process_group, signal) } == 0 {
        return Ok(());
    }

    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        // The group is gone entirely.
        Some(libc::ESRCH) => Ok(()),
        // macOS answers EPERM, not ESRCH, for a group whose only members
        // are unreaped zombies: signals cannot be posted to a zombie, and
        // the group still exists through it, so the kernel reports the
        // delivery as refused. The leader is deliberately kept unreaped to
        // pin the group id (see [`ProcessTreeChild::interrupt`]), so an
        // engine that exits just as a stop arrives lands exactly here.
        // Nothing is left to signal; the caller's wait() reports the true
        // exit.
        Some(libc::EPERM) if cfg!(target_os = "macos") => Ok(()),
        _ => Err(error),
    }
}

#[cfg(unix)]
struct ProcessGroupGuard {
    process_group: Option<libc::pid_t>,
}

#[cfg(unix)]
impl ProcessGroupGuard {
    fn new(process_group: Option<libc::pid_t>) -> Self {
        Self { process_group }
    }

    fn signal(&self, signal: libc::c_int) -> io::Result<()> {
        match self.process_group {
            Some(process_group) => signal_process_group(process_group, signal),
            None => Ok(()),
        }
    }

    fn kill(&self) -> io::Result<()> {
        match self.process_group {
            Some(process_group) => kill_process_group(process_group),
            None => Ok(()),
        }
    }

    fn disarm(&mut self) {
        self.process_group = None;
    }
}

#[cfg(unix)]
impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if let Some(process_group) = self.process_group.take() {
            let _ = kill_process_group(process_group);
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::io;
    use std::mem::size_of;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::ptr;

    use tokio::process::Child;
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenThread, ResumeThread, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME,
    };

    pub(super) const CREATE_SUSPENDED_FLAG: u32 = CREATE_SUSPENDED;
    const TERMINATED_EXIT_CODE: u32 = 1;

    pub(super) struct Job {
        handle: OwnedHandle,
    }

    impl Job {
        pub(super) fn new() -> io::Result<Self> {
            // SAFETY: null attributes and name create an unnamed job owned by
            // the returned handle.
            let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
            let handle = owned_handle(raw)?;
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `limits` has the exact layout and lifetime required for
            // JobObjectExtendedLimitInformation.
            let configured = unsafe {
                SetInformationJobObject(
                    handle.as_raw_handle() as HANDLE,
                    JobObjectExtendedLimitInformation,
                    ptr::from_ref(&limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if configured == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { handle })
        }

        pub(super) fn assign_and_resume(&self, child: &Child) -> io::Result<()> {
            let pid = child
                .id()
                .ok_or_else(|| io::Error::other("spawned child has no process id"))?;
            let process = child
                .raw_handle()
                .ok_or_else(|| io::Error::other("spawned child has no process handle"))?;
            // SAFETY: both handles are live and owned for at least this call.
            let assigned = unsafe {
                AssignProcessToJobObject(self.handle.as_raw_handle() as HANDLE, process as HANDLE)
            };
            if assigned == 0 {
                return Err(io::Error::last_os_error());
            }
            resume_process_threads(pid)
        }

        pub(super) fn terminate(&self) -> io::Result<()> {
            // SAFETY: this value owns a live Job Object handle.
            let terminated = unsafe {
                TerminateJobObject(self.handle.as_raw_handle() as HANDLE, TERMINATED_EXIT_CODE)
            };
            if terminated == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    fn resume_process_threads(pid: u32) -> io::Result<()> {
        // SAFETY: the returned snapshot handle is converted to OwnedHandle.
        let raw_snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if raw_snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: INVALID_HANDLE_VALUE was rejected above.
        let snapshot = unsafe { OwnedHandle::from_raw_handle(raw_snapshot.cast()) };
        let mut entry = THREADENTRY32 {
            dwSize: size_of::<THREADENTRY32>() as u32,
            ..THREADENTRY32::default()
        };
        // SAFETY: `entry` points to writable storage of the documented size.
        let mut has_entry = unsafe {
            Thread32First(
                snapshot.as_raw_handle() as HANDLE,
                ptr::from_mut(&mut entry),
            ) != 0
        };
        let mut resumed = false;
        while has_entry {
            if entry.th32OwnerProcessID == pid {
                // SAFETY: the id came from a live system thread snapshot.
                let raw_thread =
                    unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if !raw_thread.is_null() {
                    // SAFETY: non-null thread handle returned by OpenThread.
                    let thread = unsafe { OwnedHandle::from_raw_handle(raw_thread.cast()) };
                    // SAFETY: this child was created with CREATE_SUSPENDED, so
                    // resuming its primary thread starts the assigned process.
                    let previous = unsafe { ResumeThread(thread.as_raw_handle() as HANDLE) };
                    if previous == u32::MAX {
                        return Err(io::Error::last_os_error());
                    }
                    resumed = true;
                }
            }
            // SAFETY: `entry` remains valid writable storage across calls.
            has_entry = unsafe {
                Thread32Next(
                    snapshot.as_raw_handle() as HANDLE,
                    ptr::from_mut(&mut entry),
                ) != 0
            };
        }
        resumed
            .then_some(())
            .ok_or_else(|| io::Error::other("spawned child had no resumable thread"))
    }

    fn owned_handle(raw: HANDLE) -> io::Result<OwnedHandle> {
        if raw.is_null() || raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the successful Win32 call transferred one owned handle.
        Ok(unsafe { OwnedHandle::from_raw_handle(raw.cast()) })
    }
}

/// Captured stderr carried on an incomplete turn, in bytes.
const MAX_DETAIL_STDERR_BYTES: usize = 2 * 1_024;

/// Classify how the child that ran one turn ended.
///
/// `saw_terminal` is whether the stream reported a terminal turn event.
/// `status` is `None` when the exit could not be observed — a child another
/// task already reaped, for instance.
#[must_use]
pub fn turn_outcome(status: Option<ExitStatus>, saw_terminal: bool, stderr: &str) -> TurnOutcome {
    let failure = status.filter(|status| !status.success()).map(describe_exit);
    if failure.is_none() && saw_terminal {
        return TurnOutcome::Clean;
    }
    let mut detail =
        failure.unwrap_or_else(|| "the engine exited without reporting a result".to_owned());
    let tail = stderr_tail(stderr);
    if !tail.is_empty() {
        detail.push_str(": ");
        detail.push_str(tail);
    }
    TurnOutcome::Incomplete { detail }
}

fn describe_exit(status: ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("the engine was terminated by signal {signal}");
        }
    }
    match status.code() {
        Some(code) => format!("the engine exited with status {code}"),
        None => "the engine exited abnormally".to_owned(),
    }
}

/// The tail of captured stderr — the end carries the failure, the head
/// carries startup chatter.
fn stderr_tail(stderr: &str) -> &str {
    let trimmed = stderr.trim();
    if trimmed.len() <= MAX_DETAIL_STDERR_BYTES {
        return trimmed;
    }
    let mut start = trimmed.len() - MAX_DETAIL_STDERR_BYTES;
    while start < trimmed.len() && !trimmed.is_char_boundary(start) {
        start += 1;
    }
    &trimmed[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_that_stopped_without_a_result_is_incomplete() {
        // The defect this guards: EOF on stdout was read as a completed turn,
        // so a killed or crashed child journaled as success.
        let outcome = turn_outcome(None, false, "");
        assert!(matches!(outcome, TurnOutcome::Incomplete { .. }));
        assert_eq!(turn_outcome(None, true, ""), TurnOutcome::Clean);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_exit_is_incomplete_even_after_a_terminal_event_and_carries_stderr() {
        use std::os::unix::process::ExitStatusExt;
        let outcome = turn_outcome(Some(ExitStatus::from_raw(3 << 8)), true, "  boom  ");
        match outcome {
            TurnOutcome::Incomplete { detail } => {
                assert!(detail.contains("status 3"), "{detail}");
                assert!(detail.ends_with("boom"), "{detail}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn bounded_capture_discards_bytes_beyond_the_cap() {
        let mut capture = OutputCapture::new(OutputBudget::head(8, usize::MAX));
        assert!(capture.push(b"0123456789"));
        let output = capture.finish();
        assert_eq!(output.bytes, b"01234567");
        assert!(output.truncated);
    }

    #[test]
    fn bounded_capture_does_not_retain_a_very_long_line() {
        let mut capture = OutputCapture::new(OutputBudget::head(32, 4));
        assert!(!capture.push(&[b'x'; 16]));
        assert!(capture.push(&vec![b'x'; 64 * 1_024]));
        let output = capture.finish();
        assert_eq!(output.bytes.len(), 32);
        assert!(output.bytes.iter().all(|byte| *byte == b'x'));
        assert!(output.truncated);
    }

    #[test]
    fn bounded_capture_stops_at_the_line_cap() {
        let mut capture = OutputCapture::new(OutputBudget::head(128, 2));
        assert!(capture.push(b"one\ntwo\nthree\n"));
        let output = capture.finish();
        assert_eq!(output.bytes, b"one\ntwo\n");
        assert!(output.truncated);
    }
}

#[cfg(all(test, unix))]
mod unix_process_tree_tests {
    use std::io;
    use std::path::Path;
    use std::process::Stdio;
    use std::time::Duration;

    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    use tokio::process::{ChildStdout, Command};
    use tokio::time::{sleep, timeout, Instant};

    use super::{
        spawn_process_tree, terminate_recorded_process, ProcessTreeChild, RecordedProcessReap,
    };

    const ASSERTION_TIMEOUT: Duration = Duration::from_secs(5);
    const INTERRUPT_GRACE: Duration = Duration::from_millis(50);

    #[tokio::test]
    async fn a_reused_pid_identity_does_not_signal_the_replacement_process() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "while :; do sleep 60; done"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = spawn_process_tree(&mut command).unwrap();
        let pid = i64::from(child.id().expect("root child has a pid"));
        let reused_identity = format!("{}:reused", child.process_identity());

        assert_eq!(
            terminate_recorded_process(pid, &reused_identity, Duration::from_millis(50))
                .await
                .unwrap(),
            RecordedProcessReap::Exited
        );
        assert!(
            child.try_wait().unwrap().is_none(),
            "the replacement process must not be signaled"
        );
        child.terminate().await.unwrap();
    }

    #[tokio::test]
    async fn interrupting_after_the_tree_already_exited_is_not_an_error() {
        // The leader stays unreaped to pin the group id, and macOS reports
        // EPERM for a group holding only zombies. A stop racing an engine
        // that just finished must succeed, not surface a permission error.
        let mut command = Command::new("/usr/bin/true");
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = spawn_process_tree(&mut command).unwrap();
        sleep(Duration::from_millis(300)).await;

        let status = timeout(ASSERTION_TIMEOUT, child.interrupt(INTERRUPT_GRACE))
            .await
            .expect("interrupt of an exited tree completed")
            .unwrap();
        assert!(status.success(), "true exited nonzero: {status:?}");
    }

    #[tokio::test]
    async fn interrupting_a_process_tree_stops_an_int_ignoring_descendant() {
        let (mut child, mut stdout) = spawn_shell_tree().await;

        let status = timeout(ASSERTION_TIMEOUT, child.interrupt(INTERRUPT_GRACE))
            .await
            .expect("process-tree interrupt completed")
            .unwrap();
        assert!(!status.success(), "root unexpectedly exited successfully");
        assert_pipe_reaches_eof(&mut stdout).await;
    }

    #[tokio::test]
    async fn terminating_a_process_tree_stops_its_descendant_and_closes_its_pipe() {
        let (mut child, mut stdout) = spawn_shell_tree().await;

        timeout(ASSERTION_TIMEOUT, child.terminate())
            .await
            .expect("process-tree termination completed")
            .unwrap();
        assert_pipe_reaches_eof(&mut stdout).await;
    }

    #[tokio::test]
    async fn dropping_a_process_tree_stops_its_descendant_and_closes_its_pipe() {
        let (child, mut stdout) = spawn_shell_tree().await;

        drop(child);
        assert_pipe_reaches_eof(&mut stdout).await;
    }

    #[tokio::test]
    async fn dropping_a_process_tree_stops_a_descendant_that_is_still_forking() {
        // The descendant forks a burst of pipe-holding children right after it
        // publishes its pid, so the kill below almost certainly lands while one
        // of those forks is in flight. A fork the group walk misses outlives its
        // killed parent, keeps stdout open, and the pipe never reaches EOF.
        let script = r#"
trap 'exit 130' INT
/bin/sh -c 'trap "" INT; printf "%s\n" "$$"; i=0; while [ $i -lt 40 ]; do sleep 60 & i=$((i+1)); done; wait' &
wait
"#;
        let (child, mut stdout) = spawn_tree_from(script).await;

        drop(child);
        assert_pipe_reaches_eof(&mut stdout).await;
    }

    #[tokio::test]
    async fn cancelling_interrupt_still_escalates_and_closes_descendant_pipes() {
        let (mut child, mut stdout) = spawn_shell_tree().await;

        let cancelled = timeout(
            Duration::from_millis(10),
            child.interrupt(Duration::from_secs(5)),
        )
        .await;
        assert!(cancelled.is_err(), "interrupt unexpectedly completed");
        assert_pipe_reaches_eof(&mut stdout).await;
        timeout(ASSERTION_TIMEOUT, child.wait())
            .await
            .expect("cancelled interrupt left the root running")
            .unwrap();
    }

    #[tokio::test]
    async fn cancelling_output_collection_kills_a_pipe_owning_descendant() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("descendant.pid");
        let pid_file_arg = pid_file.to_string_lossy().into_owned();
        let script = r#"
/bin/sh -c 'printf "%s\n" "$$" > "$1"; trap "" INT; while :; do sleep 60; done' descendant "$1" &
exit 0
"#;
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", script, "root", pid_file_arg.as_str()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let child = spawn_process_tree(&mut command).unwrap();
        let root = libc::pid_t::try_from(child.id().expect("root child has a pid")).unwrap();
        let descendant = wait_for_pid(&pid_file).await;
        // SAFETY: the descendant just published its live pid.
        assert_eq!(unsafe { libc::getpgid(descendant) }, root);

        let cancelled = timeout(Duration::from_millis(25), child.wait_with_output()).await;
        assert!(
            cancelled.is_err(),
            "output collection unexpectedly ignored the inherited pipe"
        );
        assert_process_exits(descendant).await;
    }

    #[tokio::test]
    async fn bounded_output_drains_stdout_and_stderr_concurrently() {
        let script = r#"
i=0
while [ $i -lt 300 ]; do
  printf 'stdout-%04d\n' "$i"
  printf 'stderr-%04d\n' "$i" >&2
  i=$((i+1))
done
"#;
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = spawn_process_tree(&mut command).unwrap();
        let output = timeout(
            ASSERTION_TIMEOUT,
            child.wait_with_bounded_output(
                super::OutputBudget::head(1_024, 64),
                super::OutputBudget::head(1_024, 64),
                false,
            ),
        )
        .await
        .expect("bounded output completed")
        .unwrap();

        assert!(output.status.success());
        assert!(output.stdout.truncated);
        assert!(output.stderr.truncated);
        assert!(String::from_utf8_lossy(&output.stdout.bytes).starts_with("stdout-0000"));
        assert!(String::from_utf8_lossy(&output.stderr.bytes).starts_with("stderr-0000"));
    }

    #[tokio::test]
    async fn bounded_output_terminates_a_producer_that_continues_after_the_cap() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "while :; do printf '0123456789abcdef'; done"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = spawn_process_tree(&mut command).unwrap();
        let output = timeout(
            ASSERTION_TIMEOUT,
            child.wait_with_bounded_output(
                super::OutputBudget::head(128, 8),
                super::OutputBudget::tail(128, 8),
                true,
            ),
        )
        .await
        .expect("output limit terminated the producer")
        .unwrap();

        assert!(output.terminated_for_output);
        assert!(output.stdout.truncated);
        assert_eq!(output.stdout.bytes.len(), 128);
        assert!(!output.status.success());
    }

    async fn spawn_shell_tree() -> (ProcessTreeChild, BufReader<ChildStdout>) {
        // The root exits when SIGINT arrives, while its descendant deliberately
        // ignores SIGINT and retains stdout. Signalling only the root recreates
        // the Grok failure: the pipe never reaches EOF and the descendant lives.
        let script = r#"
trap 'exit 130' INT
/bin/sh -c 'trap "" INT; printf "%s\n" "$$"; while :; do sleep 60; done' &
wait
"#;
        spawn_tree_from(script).await
    }

    /// Spawn `script` under a process tree and read the descendant's pid line,
    /// asserting it really is a separate process inside the owned group.
    async fn spawn_tree_from(script: &str) -> (ProcessTreeChild, BufReader<ChildStdout>) {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = spawn_process_tree(&mut command).unwrap();
        let root = libc::pid_t::try_from(child.id().expect("root child has a pid")).unwrap();
        let mut stdout = BufReader::new(child.take_stdout().expect("stdout is piped"));
        let mut line = String::new();
        timeout(ASSERTION_TIMEOUT, stdout.read_line(&mut line))
            .await
            .expect("descendant published its pid")
            .unwrap();
        let descendant = line.trim().parse::<libc::pid_t>().unwrap();
        assert_ne!(descendant, root, "test command did not create a descendant");
        // SAFETY: the descendant just published its live pid.
        let descendant_group = unsafe { libc::getpgid(descendant) };
        assert_eq!(
            descendant_group, root,
            "descendant was not placed in the root's process group"
        );

        (child, stdout)
    }

    async fn assert_pipe_reaches_eof(stdout: &mut BufReader<ChildStdout>) {
        let mut trailing = Vec::new();
        timeout(ASSERTION_TIMEOUT, stdout.read_to_end(&mut trailing))
            .await
            .expect("descendant released the stdout pipe")
            .unwrap();
    }

    async fn wait_for_pid(path: &Path) -> libc::pid_t {
        let deadline = Instant::now() + ASSERTION_TIMEOUT;
        loop {
            if let Ok(value) = tokio::fs::read_to_string(path).await {
                if let Ok(pid) = value.trim().parse() {
                    return pid;
                }
            }
            assert!(
                Instant::now() < deadline,
                "descendant did not publish its pid"
            );
            sleep(Duration::from_millis(10)).await;
        }
    }

    async fn assert_process_exits(pid: libc::pid_t) {
        let deadline = Instant::now() + ASSERTION_TIMEOUT;
        loop {
            // SAFETY: signal 0 only checks whether the pid still exists.
            let result = unsafe { libc::kill(pid, 0) };
            if result != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "descendant {pid} remained alive after output cancellation"
            );
            sleep(Duration::from_millis(10)).await;
        }
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::Duration;

    use tokio::process::Command;
    use tokio::time::{sleep, Instant};
    use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    use super::{spawn_process_tree, ProcessTreeChild};

    const DESCENDANT_TIMEOUT: Duration = Duration::from_secs(10);
    const PID_PUBLISH_TIMEOUT: Duration = Duration::from_secs(30);

    #[tokio::test]
    async fn terminating_a_process_tree_kills_its_descendant() {
        let (mut child, descendant) = spawn_powershell_tree().await;
        child.terminate().await.unwrap();
        assert_process_exits(descendant);
    }

    #[tokio::test]
    async fn dropping_a_process_tree_kills_its_descendant() {
        let (child, descendant) = spawn_powershell_tree().await;
        drop(child);
        assert_process_exits(descendant);
    }

    async fn spawn_powershell_tree() -> (ProcessTreeChild, OwnedHandle) {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("descendant.pid");
        let powershell = powershell_path();
        let ping =
            PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot is set on Windows"))
                .join("System32")
                .join("ping.exe");
        let pid_literal = format!("'{}'", pid_file.to_string_lossy().replace('\'', "''"));
        let ping_literal = format!("'{}'", ping.to_string_lossy().replace('\'', "''"));
        // Nested PowerShell startup regularly exceeded the old 10s pid wait on
        // a loaded runner. `ping.exe -t` is a lighter long-lived descendant.
        let script = format!(
            "$child = Start-Process -FilePath {ping_literal} \
             -ArgumentList @('-t','127.0.0.1') -WindowStyle Hidden -PassThru; \
             [IO.File]::WriteAllText({pid_literal}, $child.Id.ToString(), [Text.UTF8Encoding]::new($false)); \
             Wait-Process -Id $child.Id"
        );
        let mut command = Command::new(powershell);
        command
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                script.as_str(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = spawn_process_tree(&mut command).unwrap();
        let pid = wait_for_pid(&pid_file, &mut child).await;
        let descendant = open_process(pid);
        assert_eq!(
            wait_status(&descendant, Duration::ZERO),
            WAIT_TIMEOUT,
            "descendant {pid} exited before ProcessTreeChild teardown"
        );
        // The temp directory only carries the synchronization file. Keeping it
        // until the pid has been read avoids cleanup racing the parent write.
        drop(dir);
        (child, descendant)
    }

    async fn wait_for_pid(path: &Path, child: &mut ProcessTreeChild) -> u32 {
        let deadline = Instant::now() + PID_PUBLISH_TIMEOUT;
        loop {
            if let Ok(value) = tokio::fs::read_to_string(path).await {
                if let Ok(pid) = value.trim().trim_start_matches('\u{feff}').trim().parse() {
                    return pid;
                }
            }
            assert!(
                child.try_wait().ok().flatten().is_none(),
                "PowerShell ended before publishing its descendant pid"
            );
            assert!(
                Instant::now() < deadline,
                "PowerShell descendant pid was not published within {PID_PUBLISH_TIMEOUT:?}"
            );
            sleep(Duration::from_millis(25)).await;
        }
    }

    fn open_process(pid: u32) -> OwnedHandle {
        // SAFETY: read/synchronization access is requested for the pid the
        // test parent just created and published.
        let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        assert!(!raw.is_null(), "could not open descendant process {pid}");
        // SAFETY: successful OpenProcess transfers one owned handle.
        unsafe { OwnedHandle::from_raw_handle(raw.cast()) }
    }

    fn assert_process_exits(process: OwnedHandle) {
        let result = wait_status(&process, DESCENDANT_TIMEOUT);
        assert_eq!(
            result, WAIT_OBJECT_0,
            "descendant remained alive after its ProcessTreeChild ended"
        );
    }

    fn wait_status(process: &OwnedHandle, timeout: Duration) -> u32 {
        // SAFETY: `process` is a live synchronization handle and remains owned
        // for the duration of the bounded wait.
        unsafe {
            WaitForSingleObject(
                process.as_raw_handle() as HANDLE,
                timeout.as_millis() as u32,
            )
        }
    }

    fn powershell_path() -> PathBuf {
        PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot is set on Windows"))
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe")
    }
}
