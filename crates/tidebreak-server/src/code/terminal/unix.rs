//! Own the PTY session, including foreground and background job-control groups.

use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(super) struct ProcessTree {
    session: libc::pid_t,
    cancelled: AtomicBool,
    lifetime: Mutex<Lifetime>,
    #[cfg(test)]
    scans: std::sync::atomic::AtomicUsize,
}

#[derive(Clone, Copy)]
enum Lifetime {
    Owned,
    Retired,
    Uncertain,
}

impl ProcessTree {
    pub(super) fn new(pid: u32) -> io::Result<Arc<Self>> {
        let session = libc::pid_t::try_from(pid)
            .ok()
            .filter(|pid| *pid > 0)
            .ok_or_else(|| io::Error::other("terminal shell has no process identity"))?;
        Ok(Arc::new(Self {
            session,
            cancelled: AtomicBool::new(false),
            lifetime: Mutex::new(Lifetime::Owned),
            #[cfg(test)]
            scans: std::sync::atomic::AtomicUsize::new(0),
        }))
    }

    pub(super) fn reader(self: &Arc<Self>, fd: libc::c_int) -> io::Result<Box<dyn Read + Send>> {
        // SAFETY: fd belongs to the live PTY master. The duplicate is owned here.
        let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
        if duplicate < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fcntl returned a fresh descriptor which File now owns.
        let file = unsafe { File::from_raw_fd(duplicate) };
        Ok(Box::new(Reader {
            file,
            tree: Arc::clone(self),
        }))
    }

    pub(super) fn cancel_reader(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub(super) fn terminate_and_wait(&self, deadline: Instant) -> io::Result<bool> {
        let lifetime = self
            .lifetime
            .lock()
            .map_err(|_| io::Error::other("terminal lifetime lock failed"))?;
        match *lifetime {
            Lifetime::Owned => self.terminate_owned_session(deadline),
            Lifetime::Retired => Ok(true),
            Lifetime::Uncertain => Err(io::Error::other("terminal session ownership is uncertain")),
        }
    }

    /// Observe exit without releasing the shell PID, then stop its remaining
    /// jobs before reaping. The zombie leader keeps the session ID unavailable
    /// for reuse throughout cleanup. On error the caller must retain the child.
    pub(super) fn wait_and_reap(
        &self,
        child: &mut dyn portable_pty::Child,
    ) -> io::Result<portable_pty::ExitStatus> {
        {
            let lifetime = self
                .lifetime
                .lock()
                .map_err(|_| io::Error::other("terminal lifetime lock failed"))?;
            match *lifetime {
                Lifetime::Retired => return child.wait(),
                Lifetime::Uncertain => {
                    return Err(io::Error::other("terminal session ownership is uncertain"))
                }
                Lifetime::Owned => {}
            }
        }
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        loop {
            // SAFETY: session is the positive PID of the child this reaper
            // owns. WNOWAIT leaves its exit status and PID reserved for wait().
            let observed = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.session as libc::id_t,
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if observed == 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            *self
                .lifetime
                .lock()
                .map_err(|_| io::Error::other("terminal lifetime lock failed"))? =
                Lifetime::Uncertain;
            return Err(error);
        }
        let mut lifetime = self
            .lifetime
            .lock()
            .map_err(|_| io::Error::other("terminal lifetime lock failed"))?;
        match *lifetime {
            Lifetime::Uncertain => {
                return Err(io::Error::other("terminal session ownership is uncertain"))
            }
            Lifetime::Retired => return child.wait(),
            Lifetime::Owned => {}
        }
        if !self.terminate_owned_session(Instant::now() + Duration::from_secs(5))? {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "terminal session did not stop",
            ));
        }
        // Retirement and reaping share the lock used by every terminator. Once
        // wait releases the numeric PID, no caller can scan that session again.
        *lifetime = Lifetime::Retired;
        child.wait()
    }

    fn terminate_owned_session(&self, deadline: Instant) -> io::Result<bool> {
        loop {
            #[cfg(test)]
            self.scans.fetch_add(1, Ordering::Relaxed);
            let members = session_members(self.session)?;
            if members.is_empty() {
                return Ok(true);
            }
            // Freeze every observed job before killing it. Re-enumeration catches
            // children forked between the snapshot and the stop signal.
            for pid in &members {
                signal_member(self.session, *pid, libc::SIGSTOP)?;
            }
            for pid in &members {
                signal_member(self.session, *pid, libc::SIGKILL)?;
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

struct Reader {
    file: File,
    tree: Arc<ProcessTree>,
}

impl Read for Reader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            if self.tree.cancelled.load(Ordering::Acquire) {
                return Ok(0);
            }
            let mut poll = libc::pollfd {
                fd: self.file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: poll points to one initialized descriptor owned by this reader.
            let ready = unsafe { libc::poll(&mut poll, 1, 50) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if ready == 0 {
                continue;
            }
            // This is the only reader of the master, so no competing read can
            // consume the readiness before this call. EIO denotes a closed PTY.
            return match self.file.read(buffer) {
                Err(error) if error.raw_os_error() == Some(libc::EIO) => Ok(0),
                result => result,
            };
        }
    }
}

fn signal_member(session: libc::pid_t, pid: libc::pid_t, signal: libc::c_int) -> io::Result<()> {
    // SAFETY: getsid only reads process metadata. Recheck membership before
    // sending a signal so an exited/reused pid cannot target another session.
    if unsafe { libc::getsid(pid) } != session {
        return Ok(());
    }
    // SAFETY: pid is positive and still belongs to the session we spawned.
    if unsafe { libc::kill(pid, signal) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(target_os = "linux")]
fn session_members(session: libc::pid_t) -> io::Result<Vec<libc::pid_t>> {
    let mut members = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<libc::pid_t>().ok())
        else {
            continue;
        };
        let stat = match std::fs::read(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                ) =>
            {
                continue
            }
            Err(error) => return Err(error),
        };
        let (state, observed) = parse_process_stat(&stat)?;
        if observed == session && state != b'Z' {
            members.push(pid);
        }
    }
    Ok(members)
}

// The process name is arbitrary bytes and can itself contain closing
// parentheses. Only the state and session fields after its last delimiter
// have an ASCII contract; decoding the entire record rejects valid names.
#[cfg(any(target_os = "linux", test))]
fn parse_process_stat(stat: &[u8]) -> io::Result<(u8, libc::pid_t)> {
    let end = stat
        .windows(2)
        .rposition(|bytes| bytes == b") ")
        .ok_or_else(|| io::Error::other("invalid terminal process status"))?;
    let mut fields = stat[end + 2..]
        .split(u8::is_ascii_whitespace)
        .filter(|field| !field.is_empty());
    let state = fields
        .next()
        .filter(|field| field.len() == 1)
        .map(|field| field[0])
        .ok_or_else(|| io::Error::other("invalid terminal process state"))?;
    let session = fields
        .nth(2)
        .and_then(|field| std::str::from_utf8(field).ok())
        .and_then(|field| field.parse::<libc::pid_t>().ok())
        .ok_or_else(|| io::Error::other("invalid terminal process session"))?;
    Ok((state, session))
}

#[cfg(target_os = "macos")]
fn session_members(session: libc::pid_t) -> io::Result<Vec<libc::pid_t>> {
    // Grow with room for concurrent process creation; a full buffer is not a
    // complete observation and must never prove that the session is empty.
    let mut pids = vec![0_i32; 1024];
    loop {
        let bytes = (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
        // SAFETY: the buffer holds bytes writable by proc_listallpids.
        let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        if (count as usize) < pids.len() {
            pids.truncate(count as usize);
            break;
        }
        if pids.len() >= 1_048_576 {
            return Err(io::Error::other("terminal process list exceeds limit"));
        }
        pids.resize(pids.len() * 2, 0);
    }
    let mut members = Vec::new();
    for pid in pids.into_iter().filter(|pid| *pid > 0) {
        // SAFETY: getsid reads metadata for a positive pid.
        if unsafe { libc::getsid(pid) } != session {
            continue;
        }
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        // SAFETY: proc_pidinfo writes at most size bytes to the matching struct.
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if read != size {
            // An exit between getsid and proc_pidinfo is safe to ignore.
            if unsafe { libc::getsid(pid) } != session {
                continue;
            }
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a complete proc_bsdinfo was returned above.
        if unsafe { info.assume_init() }.pbi_status != libc::SZOMB {
            members.push(pid);
        }
    }
    Ok(members)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn session_members(_session: libc::pid_t) -> io::Result<Vec<libc::pid_t>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "terminal session cleanup is unavailable on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    #[test]
    fn linux_process_status_accepts_non_utf8_names_and_embedded_parentheses() {
        let stat = b"42 (worker\xff) name) R 1 42 42 0 -1 0";
        assert_eq!(parse_process_stat(stat).unwrap(), (b'R', 42));
        assert_eq!(
            parse_process_stat(b"42 (worker\xfe) Z 1 42 42 0").unwrap(),
            (b'Z', 42)
        );
        assert!(parse_process_stat(b"42 (worker) R 1 42").is_err());
        assert!(parse_process_stat(b"42 (worker) R 1 42 invalid").is_err());
    }

    #[test]
    fn an_exited_shell_retires_its_session_before_a_later_close() {
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "exit 0"]);
        // SAFETY: setsid is async-signal-safe and touches no Rust state in the
        // post-fork child. It reproduces portable-pty's separate session.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let mut child = command.spawn().unwrap();
        let tree = ProcessTree::new(child.id()).unwrap();
        assert!(tree.wait_and_reap(&mut child).unwrap().success());
        let scans = tree.scans.load(Ordering::Relaxed);
        assert!(scans > 0, "the owned session was never checked");
        assert!(tree.terminate_and_wait(Instant::now()).unwrap());
        assert_eq!(
            tree.scans.load(Ordering::Relaxed),
            scans,
            "a reaped session ID must never be scanned again"
        );
    }
}
