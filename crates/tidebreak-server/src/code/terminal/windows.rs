//! Windows terminals join a kill-on-close job before their first instruction.
//!
//! The sole output reader polls pipe availability so cancellation never relies
//! on closing another thread's handle. Its handle closes before ConPTY does.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{Child, ChildKiller, ExitStatus, PtySize};
use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_NO_DATA, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::SearchPathW;
use windows_sys::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, UpdateProcThreadAttribute, WaitForSingleObject,
    CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_JOB_LIST,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

const PIPE_POLL: Duration = Duration::from_millis(10);

pub(super) struct Spawned {
    pub master: Master,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    pub child: Box<dyn Child + Send + Sync>,
    pub tree: Arc<ProcessTree>,
}

pub(super) struct Master {
    console: Arc<Console>,
    size: Mutex<PtySize>,
    tree: Arc<ProcessTree>,
}

impl Master {
    pub fn resize(&self, size: PtySize) -> io::Result<()> {
        let mut current = self
            .size
            .lock()
            .map_err(|_| io::Error::other("terminal size lock failed"))?;
        // SAFETY: the Arc owns the console throughout the call; size is validated.
        hresult(unsafe { ResizePseudoConsole(self.console.0, coordinates(size)?) })?;
        *current = size;
        Ok(())
    }
}

impl Drop for Master {
    fn drop(&mut self) {
        // Reader keeps the console alive until it has closed the output pipe.
        let _ = self.tree.terminate();
        self.tree.cancel_reader();
    }
}

struct Console(HPCON);

impl Drop for Console {
    fn drop(&mut self) {
        // SAFETY: this value owns the console; no reader handle remains when
        // the last Arc drops. Closing the pipe first avoids ConPTY drain deadlock.
        unsafe { ClosePseudoConsole(self.0) };
    }
}

#[derive(Debug, Default)]
struct ReaderState {
    stopped: Mutex<bool>,
    changed: Condvar,
    done: AtomicBool,
}

#[derive(Debug)]
pub(super) struct ProcessTree {
    job: OwnedHandle,
    reader: Arc<ReaderState>,
}

impl ProcessTree {
    fn new() -> io::Result<Self> {
        // SAFETY: null arguments create an unnamed, non-inheritable job.
        let job = owned(unsafe { CreateJobObjectW(ptr::null(), ptr::null()) })?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: limits has the layout and size required for this information class.
        win32(unsafe {
            SetInformationJobObject(
                raw(&job),
                JobObjectExtendedLimitInformation,
                ptr::from_ref(&limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        })?;
        Ok(Self {
            job,
            reader: Arc::new(ReaderState::default()),
        })
    }

    fn terminate(&self) -> io::Result<()> {
        // SAFETY: this value owns a live job handle. No breakaway flag is enabled.
        win32(unsafe { TerminateJobObject(raw(&self.job), 1) })
    }

    pub fn terminate_and_wait(&self, deadline: Instant) -> io::Result<bool> {
        self.terminate()?;
        loop {
            if self.active_processes()? == 0 {
                return Ok(true);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            std::thread::sleep(PIPE_POLL.min(deadline.saturating_duration_since(now)));
        }
    }

    pub fn cancel_reader(&self) {
        let mut stopped = self
            .reader
            .stopped
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *stopped = true;
        self.reader.changed.notify_all();
    }

    pub fn reader_done(&self) -> bool {
        self.reader.done.load(Ordering::Acquire)
    }

    fn active_processes(&self) -> io::Result<u32> {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: info is writable storage of the exact queried type and size.
        win32(unsafe {
            QueryInformationJobObject(
                raw(&self.job),
                JobObjectBasicAccountingInformation,
                ptr::from_mut(&mut info).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                ptr::null_mut(),
            )
        })?;
        Ok(info.ActiveProcesses)
    }
}

struct Reader {
    file: Option<File>,
    // Keeps ConPTY alive until the owned read handle closes, including cancellation.
    _console: Arc<Console>,
    state: Arc<ReaderState>,
}

impl Reader {
    fn close(&mut self) {
        self.file.take();
        self.state.done.store(true, Ordering::Release);
        self.state.changed.notify_all();
    }
}

impl Read for Reader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            let stopped = self
                .state
                .stopped
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if *stopped || self.file.is_none() {
                drop(stopped);
                self.close();
                return Ok(0);
            }
            drop(stopped);
            let mut available = 0;
            // SAFETY: this is the only reader of this pipe. Peek does not consume
            // bytes; read below requests no more than the available byte count.
            let peeked = unsafe {
                PeekNamedPipe(
                    self.file
                        .as_ref()
                        .expect("open reader")
                        .as_raw_handle()
                        .cast(),
                    ptr::null_mut(),
                    0,
                    ptr::null_mut(),
                    &mut available,
                    ptr::null_mut(),
                )
            };
            if peeked == 0 {
                let error = io::Error::last_os_error();
                self.close();
                return match error.raw_os_error().map(|value| value as u32) {
                    Some(ERROR_BROKEN_PIPE | ERROR_NO_DATA) => Ok(0),
                    _ => Err(error),
                };
            }
            if available > 0 {
                let length = bytes.len().min(available as usize);
                let result = self
                    .file
                    .as_mut()
                    .expect("open reader")
                    .read(&mut bytes[..length]);
                if matches!(&result, Ok(0) | Err(_)) {
                    self.close();
                }
                return result;
            }
            let stopped = self
                .state
                .stopped
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let _waited = self
                .state
                .changed
                .wait_timeout_while(stopped, PIPE_POLL, |stopped| !*stopped)
                .unwrap_or_else(|error| error.into_inner());
        }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.close();
    }
}

#[derive(Debug)]
struct Process {
    handle: OwnedHandle,
    pid: u32,
    tree: Arc<ProcessTree>,
}

#[derive(Debug)]
struct Killer(Arc<ProcessTree>);

impl ChildKiller for Killer {
    fn kill(&mut self) -> io::Result<()> {
        self.0.terminate()
    }
    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(Self(self.0.clone()))
    }
}

impl ChildKiller for Process {
    fn kill(&mut self) -> io::Result<()> {
        self.tree.terminate()
    }
    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(Killer(self.tree.clone()))
    }
}

impl Child for Process {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        // SAFETY: handle identifies the process created by this module.
        match unsafe { WaitForSingleObject(raw(&self.handle), 0) } {
            WAIT_OBJECT_0 => self.exit_status().map(Some),
            WAIT_TIMEOUT => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }
    fn wait(&mut self) -> io::Result<ExitStatus> {
        // SAFETY: handle remains owned throughout this blocking wait.
        if unsafe { WaitForSingleObject(raw(&self.handle), INFINITE) } != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        self.exit_status()
    }
    fn process_id(&self) -> Option<u32> {
        Some(self.pid)
    }
    fn as_raw_handle(&self) -> Option<RawHandle> {
        Some(self.handle.as_raw_handle())
    }
}

impl Process {
    fn exit_status(&self) -> io::Result<ExitStatus> {
        let mut code = 0;
        // SAFETY: code is writable; the process has already signaled completion.
        win32(unsafe { GetExitCodeProcess(raw(&self.handle), &mut code) })?;
        Ok(ExitStatus::with_exit_code(code))
    }
}

pub(super) fn spawn(
    shell: &Path,
    cwd: &Path,
    cols: u16,
    rows: u16,
    env: &[(&str, &str)],
) -> io::Result<Spawned> {
    spawn_with_args(shell, &[], cwd, cols, rows, env)
}

fn spawn_with_args(
    shell: &Path,
    args: &[&OsStr],
    cwd: &Path,
    cols: u16,
    rows: u16,
    env: &[(&str, &str)],
) -> io::Result<Spawned> {
    let size = PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    };
    let executable = resolve_executable(shell)?;
    let mut command = quoted_command(&executable, args)?;
    let cwd = wide(cwd.as_os_str())?;
    let environment = environment(env)?;
    let tree = Arc::new(ProcessTree::new()?);
    let (input_read, input_write) = pipe()?;
    let (output_read, output_write) = pipe()?;
    let mut console = 0;
    // SAFETY: the pipe handles remain live during creation and size is validated.
    hresult(unsafe {
        CreatePseudoConsole(
            coordinates(size)?,
            raw(&input_read),
            raw(&output_write),
            0,
            &mut console,
        )
    })?;
    let console = Arc::new(Console(console));
    // ConPTY owns duplicates. Keeping these copies would prevent EOF detection.
    drop(input_read);
    drop(output_write);
    // Keep failure cleanup in the same order as normal shutdown: the output
    // pipe closes before the last console reference, even if attributes fail.
    let reader = Reader {
        file: Some(File::from(output_read)),
        _console: console.clone(),
        state: tree.reader.clone(),
    };
    let mut attributes = Attributes::new()?;
    attributes.set(
        PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
        console.0 as *const _,
        size_of::<HPCON>(),
    )?;
    let jobs = [raw(&tree.job)];
    attributes.set(
        PROC_THREAD_ATTRIBUTE_JOB_LIST,
        jobs.as_ptr().cast(),
        size_of_val(&jobs),
    )?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
    startup.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
    startup.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
    startup.lpAttributeList = attributes.pointer();
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: all buffers and both attribute payloads stay live until creation
    // completes. JOB_LIST assigns the job atomically before any child code runs.
    // No fallback starts an uncontained process if this call fails.
    win32(unsafe {
        CreateProcessW(
            executable.as_ptr(),
            command.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast(),
            cwd.as_ptr(),
            &startup.StartupInfo,
            &mut info,
        )
    })?;
    // SAFETY: successful CreateProcessW transfers both handles to this caller.
    let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess.cast()) };
    let thread = unsafe { OwnedHandle::from_raw_handle(info.hThread.cast()) };
    drop(thread);
    Ok(Spawned {
        master: Master {
            console,
            size: Mutex::new(size),
            tree: tree.clone(),
        },
        reader: Box::new(reader),
        writer: Box::new(File::from(input_write)),
        child: Box::new(Process {
            handle: process,
            pid: info.dwProcessId,
            tree: tree.clone(),
        }),
        tree,
    })
}

struct Attributes(Vec<usize>);

impl Attributes {
    fn new() -> io::Result<Self> {
        let mut size = 0;
        // SAFETY: the first call only obtains the allocation size.
        unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 2, 0, &mut size) };
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![0usize; size.div_ceil(size_of::<usize>())];
        // SAFETY: usize storage has pointer alignment and at least size bytes.
        win32(unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), 2, 0, &mut size)
        })?;
        Ok(Self(storage))
    }
    fn pointer(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.0.as_mut_ptr().cast()
    }
    fn set(
        &mut self,
        attribute: u32,
        value: *const std::ffi::c_void,
        bytes: usize,
    ) -> io::Result<()> {
        // SAFETY: callers retain the correctly sized attribute payload through spawn.
        win32(unsafe {
            UpdateProcThreadAttribute(
                self.pointer(),
                0,
                attribute as usize,
                value,
                bytes,
                ptr::null_mut(),
                ptr::null(),
            )
        })
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: this is the initialized list owned by this allocation.
        unsafe { DeleteProcThreadAttributeList(self.pointer()) };
    }
}

fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let (mut read, mut write) = (ptr::null_mut(), ptr::null_mut());
    // SAFETY: null security attributes create non-inheritable handles.
    win32(unsafe { CreatePipe(&mut read, &mut write, ptr::null(), 0) })?;
    Ok((owned(read)?, owned(write)?))
}

fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: caller transfers a freshly created handle after checking success.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle.cast()) })
}

fn raw(handle: &OwnedHandle) -> HANDLE {
    handle.as_raw_handle().cast()
}
fn win32(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn hresult(result: i32) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::other(format!(
            "Windows terminal failed (HRESULT {result:#x})"
        )))
    } else {
        Ok(())
    }
}
fn coordinates(size: PtySize) -> io::Result<COORD> {
    if size.cols == 0 || size.rows == 0 {
        return Err(io::Error::other("terminal size must be positive"));
    }
    Ok(COORD {
        X: i16::try_from(size.cols).map_err(|_| io::Error::other("terminal is too wide"))?,
        Y: i16::try_from(size.rows).map_err(|_| io::Error::other("terminal is too tall"))?,
    })
}
fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "terminal argument contains NUL",
        ));
    }
    value.push(0);
    Ok(value)
}
fn resolve_executable(shell: &Path) -> io::Result<Vec<u16>> {
    let name = wide(shell.as_os_str())?;
    let extension = wide(OsStr::new(".exe"))?;
    // SAFETY: the first call reports the required UTF-16 buffer length.
    let needed = unsafe {
        SearchPathW(
            ptr::null(),
            name.as_ptr(),
            extension.as_ptr(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut resolved = vec![0u16; needed as usize];
    // SAFETY: resolved has the reported capacity and all input strings are NUL terminated.
    let length = unsafe {
        SearchPathW(
            ptr::null(),
            name.as_ptr(),
            extension.as_ptr(),
            needed,
            resolved.as_mut_ptr(),
            ptr::null_mut(),
        )
    };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length >= needed {
        return Err(io::Error::other(
            "terminal executable path changed during lookup",
        ));
    }
    resolved.truncate(length as usize + 1);
    Ok(resolved)
}
fn quoted_command(executable: &[u16], args: &[&OsStr]) -> io::Result<Vec<u16>> {
    let mut command = Vec::new();
    for argument in std::iter::once(OsString::from_wide(&executable[..executable.len() - 1]))
        .chain(args.iter().map(|arg| arg.to_os_string()))
    {
        let argument = wide(&argument)?;
        if !command.is_empty() {
            command.push(b' ' as u16);
        }
        command.push(b'"' as u16);
        let mut slashes = 0;
        for &unit in &argument[..argument.len() - 1] {
            if unit == b'\\' as u16 {
                slashes += 1;
                continue;
            }
            command.extend(std::iter::repeat_n(
                b'\\' as u16,
                if unit == b'"' as u16 {
                    2 * slashes + 1
                } else {
                    slashes
                },
            ));
            command.push(unit);
            slashes = 0;
        }
        command.extend(std::iter::repeat_n(b'\\' as u16, 2 * slashes));
        command.push(b'"' as u16);
    }
    command.push(0);
    Ok(command)
}
fn environment(overrides: &[(&str, &str)]) -> io::Result<Vec<u16>> {
    let mut values: BTreeMap<OsString, (OsString, OsString)> = std::env::vars_os()
        .map(|(key, value)| (key.to_ascii_uppercase(), (key, value)))
        .collect();
    for &(key, value) in overrides {
        values.insert(
            OsString::from(key).to_ascii_uppercase(),
            (key.into(), value.into()),
        );
    }
    let mut block = Vec::new();
    for (key, value) in values.into_values() {
        let key = wide(&key)?;
        block.extend_from_slice(&key[..key.len() - 1]);
        block.push(b'=' as u16);
        block.extend(wide(&value)?);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_preserves_trailing_backslashes_and_embedded_quotes() {
        let executable = wide(OsStr::new(r"C:\Program Files\shell.exe")).unwrap();
        let command = quoted_command(
            &executable,
            &[OsStr::new("a\\\"b"), OsStr::new("C:\\end\\")],
        )
        .unwrap();
        assert_eq!(
            OsString::from_wide(&command[..command.len() - 1]),
            OsStr::new("\"C:\\Program Files\\shell.exe\" \"a\\\\\\\"b\" \"C:\\end\\\\\"")
        );
    }

    #[test]
    fn canceling_a_quiet_reader_closes_its_pipe() {
        let mut spawned = spawn_with_args(
            Path::new("powershell.exe"),
            &[
                OsStr::new("-NoProfile"),
                OsStr::new("-Command"),
                OsStr::new("Start-Sleep -Seconds 30"),
            ],
            &std::env::temp_dir(),
            80,
            24,
            &[],
        )
        .unwrap();
        let reader = std::thread::spawn(move || {
            let mut bytes = [0u8; 1024];
            while spawned.reader.read(&mut bytes).unwrap() > 0 {}
        });
        spawned.tree.cancel_reader();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !reader.is_finished() && Instant::now() < deadline {
            std::thread::sleep(PIPE_POLL);
        }
        let finished = reader.is_finished();
        let closed = spawned.tree.reader_done();
        assert!(spawned.tree.terminate_and_wait(deadline).unwrap());
        assert!(finished, "reader did not honor cancellation");
        reader.join().unwrap();
        assert!(
            closed,
            "reader reported completion before releasing its pipe"
        );
    }

    #[test]
    fn the_job_contains_descendants_from_process_creation() {
        // The root exits immediately after starting its child. Shell-only
        // termination cannot satisfy this test once the root has been reaped.
        let script = "Start-Process powershell.exe -NoNewWindow -ArgumentList \
                      '-NoProfile','-Command','Start-Sleep -Seconds 30'";
        let mut spawned = spawn_with_args(
            Path::new("powershell.exe"),
            &[
                OsStr::new("-NoProfile"),
                OsStr::new("-Command"),
                OsStr::new(script),
            ],
            &std::env::temp_dir(),
            80,
            24,
            &[],
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut root_exited = false;
        while Instant::now() < deadline {
            if spawned.child.try_wait().unwrap().is_some() {
                root_exited = true;
                break;
            }
            std::thread::sleep(PIPE_POLL);
        }
        let contained = spawned.tree.active_processes().unwrap() > 0;
        assert!(spawned
            .tree
            .terminate_and_wait(Instant::now() + Duration::from_secs(5))
            .unwrap());
        assert!(root_exited, "the terminal shell did not exit");
        assert!(
            contained,
            "the descendant did not remain in the terminal job"
        );
        assert_eq!(spawned.tree.active_processes().unwrap(), 0);
    }
}
