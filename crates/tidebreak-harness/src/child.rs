//! Bookkeeping for adapters that run one engine child per turn.
//!
//! Two facts the session worker needs, and a pid read at turn boundaries
//! cannot give it: the pid *while* the turn is in flight (that is the whole
//! window a crash can orphan a child in), and how the child ended once it is
//! gone (an EOF on stdout is not a completed turn).

use std::io;
use std::process::{ExitStatus, Output};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::watch;
#[cfg(unix)]
use tokio::time::sleep;

use crate::TurnOutcome;

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
        return Ok(ProcessTreeChild {
            child: Some(child),
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
        let process_group = match child.id().and_then(|pid| libc::pid_t::try_from(pid).ok()) {
            Some(process_group) => process_group,
            None => {
                let _ = child.start_kill();
                return Err(io::Error::other("spawned child has no valid process id"));
            }
        };
        Ok(ProcessTreeChild {
            child: Some(child),
            process_group: Some(process_group),
        })
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        Ok(ProcessTreeChild {
            child: Some(command.spawn()?),
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
            signal_process_group(process_group, libc::SIGKILL)?;
            self.process_group = None;
            Ok(())
        }
        #[cfg(all(not(unix), not(windows)))]
        {
            self.child_mut().start_kill()
        }
    }
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
        self.signal(libc::SIGKILL)
    }

    fn disarm(&mut self) {
        self.process_group = None;
    }
}

#[cfg(unix)]
impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if let Some(process_group) = self.process_group.take() {
            let _ = signal_process_group(process_group, libc::SIGKILL);
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

    use super::{spawn_process_tree, ProcessTreeChild};

    const ASSERTION_TIMEOUT: Duration = Duration::from_secs(5);
    const INTERRUPT_GRACE: Duration = Duration::from_millis(50);

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

    async fn spawn_shell_tree() -> (ProcessTreeChild, BufReader<ChildStdout>) {
        // The root exits when SIGINT arrives, while its descendant deliberately
        // ignores SIGINT and retains stdout. Signalling only the root recreates
        // the Grok failure: the pipe never reaches EOF and the descendant lives.
        let script = r#"
trap 'exit 130' INT
/bin/sh -c 'trap "" INT; printf "%s\n" "$$"; while :; do sleep 60; done' &
wait
"#;
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
