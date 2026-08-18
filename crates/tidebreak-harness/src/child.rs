//! Bookkeeping for adapters that run one engine child per turn.
//!
//! Two facts the session worker needs, and a pid read at turn boundaries
//! cannot give it: the pid *while* the turn is in flight (that is the whole
//! window a crash can orphan a child in), and how the child ended once it is
//! gone (an EOF on stdout is not a completed turn).

use std::io;
use std::process::{ExitStatus, Output};
use std::time::Duration;

use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::watch;
#[cfg(unix)]
use tokio::time::timeout;

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

/// One child and the operating-system process tree it owns.
///
/// Windows children are created suspended, assigned to a Job Object configured
/// with `KILL_ON_JOB_CLOSE`, then resumed. That closes the usual spawn/assign
/// race where a wrapper such as `npm.cmd` could create a descendant before it
/// became part of the job. Dropping this value terminates the job tree.
///
/// Unix keeps Tokio's immediate-child kill-on-drop behavior. [`Self::interrupt`]
/// sends SIGINT first and escalates after the caller's grace period.
pub struct ProcessTreeChild {
    child: Option<Child>,
    #[cfg(windows)]
    job: windows::Job,
}

/// Spawn `command` with process-tree ownership.
///
/// Callers may await [`ProcessTreeChild::wait_with_output`] inside a timeout:
/// cancelling that future drops the owned child and terminates its Windows Job
/// Object tree.
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
    #[cfg(not(windows))]
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

    /// Wait for the root child to exit.
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child_mut().wait().await
    }

    /// Read a completed root child's exit status without waiting.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child_mut().try_wait()
    }

    /// Wait for the root child and collect its configured stdout and stderr.
    ///
    /// Dropping this future before completion drops `self`, which terminates
    /// the owned Windows process tree.
    pub async fn wait_with_output(mut self) -> io::Result<Output> {
        self.child
            .take()
            .expect("process child is present")
            .wait_with_output()
            .await
    }

    /// Ask the process to stop, escalating to tree termination after `grace`.
    ///
    /// Windows has no reliable console-control channel for GUI-spawned batch
    /// shims, so interruption terminates the Job Object immediately. Unix
    /// sends SIGINT to the root child first and preserves the existing grace
    /// period before escalating.
    pub async fn interrupt(&mut self, grace: Duration) -> io::Result<ExitStatus> {
        #[cfg(unix)]
        if let Some(pid) = self.id() {
            // SAFETY: the pid belongs to the child held by this value.
            let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGINT) };
            if let Ok(status) = timeout(grace, self.wait()).await {
                return status;
            }
        }
        #[cfg(not(unix))]
        let _ = grace;

        self.terminate().await
    }

    /// Terminate the entire owned tree and wait for the root child.
    pub async fn terminate(&mut self) -> io::Result<ExitStatus> {
        self.terminate_tree()?;
        self.wait().await
    }

    fn terminate_tree(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        {
            self.job.terminate()
        }
        #[cfg(not(windows))]
        {
            self.child_mut().start_kill()
        }
    }
}

impl Drop for ProcessTreeChild {
    fn drop(&mut self) {
        if self.child.is_some() {
            let _ = self.terminate_tree();
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
        let pid_literal = format!("'{}'", pid_file.to_string_lossy().replace('\'', "''"));
        let script = format!(
            "$child = Start-Process -FilePath (Join-Path $PSHOME 'powershell.exe') \
             -ArgumentList @('-NoLogo','-NoProfile','-NonInteractive','-Command',\
             'Start-Sleep -Seconds 120') -PassThru; \
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
        let child = spawn_process_tree(&mut command).unwrap();
        let pid = wait_for_pid(&pid_file).await;
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

    async fn wait_for_pid(path: &Path) -> u32 {
        let deadline = Instant::now() + DESCENDANT_TIMEOUT;
        loop {
            if let Ok(value) = tokio::fs::read_to_string(path).await {
                if let Ok(pid) = value.trim().trim_start_matches('\u{feff}').trim().parse() {
                    return pid;
                }
            }
            assert!(
                Instant::now() < deadline,
                "PowerShell descendant pid was not published within {DESCENDANT_TIMEOUT:?}"
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
