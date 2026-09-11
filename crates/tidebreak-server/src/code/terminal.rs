//! Auxiliary workspace terminals: a PTY plus a bounded in-memory ring.
//!
//! Bytes are ephemeral. They are not journaled, not persisted, and vanish
//! when this process does. The harness crate has no PTY dependency; this
//! module is the only place one is used.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, Weak};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(unix)]
use unix::ProcessTree;
#[cfg(windows)]
use windows::ProcessTree;
#[cfg(unix)]
type TerminalMaster = Box<dyn MasterPty + Send>;
#[cfg(windows)]
type TerminalMaster = windows::Master;
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use portable_pty::PtySize;
#[cfg(unix)]
use portable_pty::{native_pty_system, CommandBuilder, MasterPty};
use tidebreak_core::{CodeTerminalId, OwnerId, WorkspaceId};
use tokio::sync::broadcast;

/// Cap on live shells per workspace. A terminal is a convenience, not a data plane.
pub const MAX_TERMINALS_PER_WORKSPACE: usize = 8;

/// Retained output per terminal. Overflow drops the oldest bytes; a reader
/// whose cursor sits behind the ring sees an inline truncation marker.
pub const TERMINAL_RING_BYTES: usize = 256 * 1024;

/// Upper bound on one cursor-pull response so a client cannot drain the ring
/// in a single unbounded read.
pub const MAX_TERMINAL_READ_BYTES: usize = 32 * 1024;

/// Upper bound on one keystroke/write POST.
pub const MAX_TERMINAL_WRITE_BYTES: usize = 4 * 1024;

/// Activity notices are coalesced to this granularity.
pub const TERMINAL_NOTICE_COALESCE: Duration = Duration::from_millis(32);

/// Inserted at the front of a read whose cursor has fallen off the ring.
pub const TRUNCATION_MARKER: &[u8] = b"\r\n[output truncated]\r\n";

const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;
const MIN_SIZE: u16 = 1;
const MAX_SIZE: u16 = 512;
const NOTICE_BUFFER: usize = 64;
const TERMINAL_EXIT_GRACE: Duration = Duration::from_secs(5);

/// In-memory process-wide registry of live auxiliary terminals.
pub struct TerminalHub {
    inner: Mutex<HubInner>,
    /// One notice channel per owner. Terminal activity rides
    /// `/updates`, so it is partitioned the same way the digests are:
    /// a subscriber's receiver only ever carries its own owner's notices.
    notices: Mutex<HashMap<OwnerId, broadcast::Sender<TerminalNotice>>>,
}

struct HubInner {
    by_id: HashMap<CodeTerminalId, Arc<Mutex<LiveTerminal>>>,
    by_workspace: HashMap<WorkspaceId, Vec<CodeTerminalId>>,
    reservations: HashMap<WorkspaceId, usize>,
}

struct LiveTerminal {
    id: CodeTerminalId,
    owner: OwnerId,
    workspace_id: WorkspaceId,
    ring: ByteRing,
    cols: u16,
    rows: u16,
    /// The shell is gone. Nothing can be typed at it any more, which is what
    /// a snapshot reports and what `write` and `resize` refuse on.
    ended: bool,
    /// A PTY reader thread is still feeding this ring, so bytes can still
    /// arrive after `ended`. The reaper flips `ended` the moment the child
    /// exits, which is before the reader has drained what the shell wrote on
    /// its way out. A read reports `ended` only once both have settled, so a
    /// client that stops polling there has seen every byte.
    producing: bool,
    created_at: DateTime<Utc>,
    writer: Option<Box<dyn Write + Send>>,
    master: Option<TerminalMaster>,
    tree: Option<Arc<ProcessTree>>,
    reader_exit: Arc<TerminalExit>,
    reader_thread: Option<thread::JoinHandle<()>>,
    reaper_thread: Option<thread::JoinHandle<()>>,
    exit: Arc<TerminalExit>,
    coalesce: Coalesce,
}

struct TerminalExit {
    done: Mutex<Option<bool>>,
    changed: Condvar,
}

struct Coalesce {
    dirty: bool,
    scheduled: bool,
    quiet_until: Instant,
}

/// Unsequenced activity notice. Published on the workspace bus; never journaled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalNotice {
    pub workspace_id: WorkspaceId,
    pub terminal_id: CodeTerminalId,
}

/// Snapshot a route can serialize.
#[derive(Debug, Clone)]
pub struct TerminalSnapshot {
    pub id: CodeTerminalId,
    pub workspace_id: WorkspaceId,
    pub cols: u16,
    pub rows: u16,
    pub ended: bool,
    pub created_at: DateTime<Utc>,
}

/// One cursor-pull response.
#[derive(Debug, Clone)]
pub struct TerminalRead {
    pub data: Vec<u8>,
    pub next_cursor: u64,
    pub overflow: bool,
    pub truncated: bool,
    pub ended: bool,
}

#[derive(Debug)]
pub enum TerminalError {
    WorkspaceCap,
    WriteTooLarge,
    Ended,
    NotFound,
    InvalidSize,
    Spawn(String),
    Io(String),
}

impl TerminalHub {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HubInner {
                by_id: HashMap::new(),
                by_workspace: HashMap::new(),
                reservations: HashMap::new(),
            }),
            notices: Mutex::new(HashMap::new()),
        }
    }

    fn notices_sender(&self, owner: &OwnerId) -> broadcast::Sender<TerminalNotice> {
        self.notices
            .lock()
            .expect("terminal notice bus")
            .entry(owner.clone())
            .or_insert_with(|| broadcast::channel(NOTICE_BUFFER).0)
            .clone()
    }

    /// Subscribe to one owner's terminal activity.
    pub fn subscribe(&self, owner: &OwnerId) -> broadcast::Receiver<TerminalNotice> {
        self.notices_sender(owner).subscribe()
    }

    pub fn open(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        cwd: &Path,
        cols: Option<u16>,
        rows: Option<u16>,
    ) -> Result<TerminalSnapshot, TerminalError> {
        let cols = clamp_size(cols.unwrap_or(DEFAULT_COLS))?;
        let rows = clamp_size(rows.unwrap_or(DEFAULT_ROWS))?;
        let reservation = self.reserve_slot(workspace_id)?;
        let spawned = spawn_pty(cwd, cols, rows)?;
        let id = CodeTerminalId::new();
        let live = LiveTerminal {
            id,
            owner: owner.clone(),
            workspace_id,
            ring: ByteRing::new(TERMINAL_RING_BYTES),
            cols,
            rows,
            ended: false,
            producing: true,
            created_at: Utc::now(),
            writer: Some(spawned.writer),
            master: Some(spawned.master),
            tree: Some(spawned.tree),
            reader_exit: Arc::new(TerminalExit::new(false)),
            reader_thread: None,
            reaper_thread: None,
            exit: Arc::new(TerminalExit::new(false)),
            coalesce: Coalesce::new(),
        };
        let handle = Arc::new(Mutex::new(live));
        let notices = self.notices_sender(owner);
        let reader_thread = start_reader(handle.clone(), notices.clone(), spawned.reader);
        let reaper_thread = start_reaper(
            handle.clone(),
            notices,
            spawned.child,
            handle.lock().expect("terminal").exit.clone(),
        );
        {
            let mut live = handle.lock().expect("terminal");
            live.reader_thread = Some(reader_thread);
            live.reaper_thread = Some(reaper_thread);
        }
        reservation.insert(id, handle.clone());
        Ok(lock_snapshot(&handle))
    }

    /// Open a ring with no PTY. Used by tests that must not spawn a shell.
    #[cfg(test)]
    pub fn open_memory(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalSnapshot, TerminalError> {
        let cols = clamp_size(cols)?;
        let rows = clamp_size(rows)?;
        let reservation = self.reserve_slot(workspace_id)?;
        let id = CodeTerminalId::new();
        let live = LiveTerminal {
            id,
            owner: owner.clone(),
            workspace_id,
            ring: ByteRing::new(TERMINAL_RING_BYTES),
            cols,
            rows,
            ended: false,
            producing: false,
            created_at: Utc::now(),
            writer: None,
            master: None,
            tree: None,
            reader_exit: Arc::new(TerminalExit::new(true)),
            reader_thread: None,
            reaper_thread: None,
            exit: Arc::new(TerminalExit::new(true)),
            coalesce: Coalesce::new(),
        };
        let handle = Arc::new(Mutex::new(live));
        reservation.insert(id, handle.clone());
        Ok(lock_snapshot(&handle))
    }

    pub fn list(&self, workspace_id: WorkspaceId) -> Vec<TerminalSnapshot> {
        let inner = self.inner.lock().expect("terminal hub");
        let ids = inner
            .by_workspace
            .get(&workspace_id)
            .cloned()
            .unwrap_or_default();
        ids.iter()
            .filter_map(|id| inner.by_id.get(id).map(lock_snapshot))
            .collect()
    }

    #[cfg(test)]
    pub fn get(&self, workspace_id: WorkspaceId, id: CodeTerminalId) -> Option<TerminalSnapshot> {
        let inner = self.inner.lock().expect("terminal hub");
        let handle = inner.by_id.get(&id)?;
        let snap = lock_snapshot(handle);
        if snap.workspace_id != workspace_id {
            return None;
        }
        Some(snap)
    }

    pub fn read(&self, workspace_id: WorkspaceId, id: CodeTerminalId, cursor: u64) -> TerminalRead {
        let Some(handle) = self.handle(workspace_id, id) else {
            return TerminalRead {
                data: Vec::new(),
                next_cursor: cursor,
                overflow: false,
                truncated: false,
                ended: true,
            };
        };
        let live = handle.lock().expect("terminal");
        let (data, next_cursor, overflow, truncated) =
            live.ring.read(cursor, MAX_TERMINAL_READ_BYTES);
        TerminalRead {
            data,
            next_cursor,
            overflow,
            truncated,
            // `ended` is what stops a client polling, so it has to mean the
            // cursor is final — not just that the shell is gone while its
            // last line is still on its way out of the PTY.
            ended: live.ended && !live.producing,
        }
    }

    pub fn write(
        &self,
        workspace_id: WorkspaceId,
        id: CodeTerminalId,
        bytes: &[u8],
    ) -> Result<(), TerminalError> {
        if bytes.len() > MAX_TERMINAL_WRITE_BYTES {
            return Err(TerminalError::WriteTooLarge);
        }
        let handle = self
            .handle(workspace_id, id)
            .ok_or(TerminalError::NotFound)?;
        let mut live = handle.lock().expect("terminal");
        if live.ended {
            return Err(TerminalError::Ended);
        }
        if let Some(writer) = live.writer.as_mut() {
            writer
                .write_all(bytes)
                .and_then(|()| writer.flush())
                .map_err(|err| TerminalError::Io(err.to_string()))?;
        } else {
            // Memory-backed terminals echo writes into the ring so tests can
            // drive the same read path the PTY reader uses.
            live.ring.write(bytes);
            let notices = self.notices_sender(&live.owner.clone());
            apply_coalesce(&mut live, &handle, &notices, workspace_id, id);
        }
        Ok(())
    }

    pub fn resize(
        &self,
        workspace_id: WorkspaceId,
        id: CodeTerminalId,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalSnapshot, TerminalError> {
        let cols = clamp_size(cols)?;
        let rows = clamp_size(rows)?;
        let handle = self
            .handle(workspace_id, id)
            .ok_or(TerminalError::NotFound)?;
        let mut live = handle.lock().expect("terminal");
        if live.ended {
            return Err(TerminalError::Ended);
        }
        if let Some(master) = live.master.as_ref() {
            master
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|err| TerminalError::Io(err.to_string()))?;
        }
        live.cols = cols;
        live.rows = rows;
        Ok(snapshot(&live))
    }

    pub fn close(
        &self,
        workspace_id: WorkspaceId,
        id: CodeTerminalId,
    ) -> Result<(), TerminalError> {
        let handle = self
            .handle(workspace_id, id)
            .ok_or(TerminalError::NotFound)?;
        shutdown(&handle, Instant::now() + TERMINAL_EXIT_GRACE)?;
        self.remove(workspace_id, id);
        Ok(())
    }

    /// Stop terminals and retain any uncertain shutdown so archive can retry it.
    pub async fn close_workspace_and_wait(&self, workspace_id: WorkspaceId) -> bool {
        let handles = {
            let inner = self.inner.lock().expect("terminal hub");
            if inner.reservations.get(&workspace_id).copied().unwrap_or(0) != 0 {
                return false;
            }
            inner
                .by_workspace
                .get(&workspace_id)
                .into_iter()
                .flatten()
                .filter_map(|id| inner.by_id.get(id).cloned())
                .collect::<Vec<_>>()
        };
        let result = tokio::task::spawn_blocking(move || {
            let deadline = Instant::now() + TERMINAL_EXIT_GRACE;
            handles
                .into_iter()
                .map(|handle| {
                    let id = handle.lock().expect("terminal").id;
                    (id, shutdown(&handle, deadline).is_ok())
                })
                .collect::<Vec<_>>()
        })
        .await;
        let Ok(results) = result else {
            return false;
        };
        let mut complete = true;
        for (id, stopped) in results {
            if stopped {
                self.remove(workspace_id, id);
            } else {
                complete = false;
            }
        }
        complete
    }

    /// Append output as if the PTY had produced it. Tests and the reader thread.
    #[cfg(any(test, feature = "test-support"))]
    pub fn push_output(&self, id: CodeTerminalId, bytes: &[u8]) {
        let handle = {
            let inner = self.inner.lock().expect("terminal hub");
            inner.by_id.get(&id).cloned()
        };
        let Some(handle) = handle else {
            return;
        };
        let mut live = handle.lock().expect("terminal");
        live.ring.write(bytes);
        let workspace_id = live.workspace_id;
        let notices = self.notices_sender(&live.owner.clone());
        apply_coalesce(&mut live, &handle, &notices, workspace_id, id);
    }

    fn reserve_slot(&self, workspace_id: WorkspaceId) -> Result<Reservation<'_>, TerminalError> {
        let mut inner = self.inner.lock().expect("terminal hub");
        let count = inner
            .by_workspace
            .get(&workspace_id)
            .map(Vec::len)
            .unwrap_or(0)
            + inner.reservations.get(&workspace_id).copied().unwrap_or(0);
        if count >= MAX_TERMINALS_PER_WORKSPACE {
            return Err(TerminalError::WorkspaceCap);
        }
        *inner.reservations.entry(workspace_id).or_default() += 1;
        Ok(Reservation {
            hub: self,
            workspace_id,
        })
    }

    fn remove(&self, workspace_id: WorkspaceId, id: CodeTerminalId) {
        let mut inner = self.inner.lock().expect("terminal hub");
        inner.by_id.remove(&id);
        if let Some(ids) = inner.by_workspace.get_mut(&workspace_id) {
            ids.retain(|existing| *existing != id);
            if ids.is_empty() {
                inner.by_workspace.remove(&workspace_id);
            }
        }
    }

    fn handle(
        &self,
        workspace_id: WorkspaceId,
        id: CodeTerminalId,
    ) -> Option<Arc<Mutex<LiveTerminal>>> {
        let inner = self.inner.lock().expect("terminal hub");
        let handle = inner.by_id.get(&id)?.clone();
        let live = handle.lock().expect("terminal");
        if live.workspace_id != workspace_id {
            return None;
        }
        drop(live);
        Some(handle)
    }
}

impl Default for TerminalHub {
    fn default() -> Self {
        Self::new()
    }
}

struct Reservation<'a> {
    hub: &'a TerminalHub,
    workspace_id: WorkspaceId,
}

impl Reservation<'_> {
    fn insert(self, id: CodeTerminalId, handle: Arc<Mutex<LiveTerminal>>) {
        let mut inner = self.hub.inner.lock().expect("terminal hub");
        inner.by_id.insert(id, handle);
        inner
            .by_workspace
            .entry(self.workspace_id)
            .or_default()
            .push(id);
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        let mut inner = self.hub.inner.lock().expect("terminal hub");
        if let Some(count) = inner.reservations.get_mut(&self.workspace_id) {
            *count -= 1;
            if *count == 0 {
                inner.reservations.remove(&self.workspace_id);
            }
        }
    }
}

fn shutdown(handle: &Arc<Mutex<LiveTerminal>>, deadline: Instant) -> Result<(), TerminalError> {
    let (tree, exit, reader_exit) = {
        let mut live = handle.lock().expect("terminal");
        live.ended = true;
        (
            live.tree.clone(),
            Arc::clone(&live.exit),
            Arc::clone(&live.reader_exit),
        )
    };
    if let Some(tree) = &tree {
        if !tree
            .terminate_and_wait(deadline)
            .map_err(|error| TerminalError::Io(error.to_string()))?
        {
            return Err(TerminalError::Io("terminal processes did not stop".into()));
        }
        tree.cancel_reader();
    }
    if !exit.wait_until(deadline) || !reader_exit.wait_until(deadline) {
        return Err(TerminalError::Io(
            "terminal shutdown did not complete".into(),
        ));
    }
    #[cfg(windows)]
    if tree.as_ref().is_some_and(|tree| !tree.reader_done()) {
        return Err(TerminalError::Io(
            "terminal reader handle remains open".into(),
        ));
    }
    let (reader, reaper, writer, master) = {
        let mut live = handle.lock().expect("terminal");
        (
            live.reader_thread.take(),
            live.reaper_thread.take(),
            live.writer.take(),
            live.master.take(),
        )
    };
    // Completion is signaled after the blocking handles are released. Join
    // outside the terminal mutex because each worker uses it on its way out.
    for worker in [reader, reaper].into_iter().flatten() {
        worker
            .join()
            .map_err(|_| TerminalError::Io("terminal worker failed".into()))?;
    }
    drop(writer);
    drop(master);
    Ok(())
}

impl TerminalExit {
    fn new(done: bool) -> Self {
        Self {
            done: Mutex::new(done.then_some(true)),
            changed: Condvar::new(),
        }
    }

    fn mark_done(&self, succeeded: bool) {
        *self.done.lock().expect("terminal exit") = Some(succeeded);
        self.changed.notify_all();
    }

    fn wait_until(&self, deadline: Instant) -> bool {
        let mut done = self.done.lock().expect("terminal exit");
        while done.is_none() {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let waited = self
                .changed
                .wait_timeout(done, deadline.saturating_duration_since(now))
                .expect("terminal exit");
            done = waited.0;
            if waited.1.timed_out() && done.is_none() {
                return false;
            }
        }
        *done == Some(true)
    }
}

struct Spawned {
    master: TerminalMaster,
    writer: Box<dyn Write + Send>,
    reader: Box<dyn Read + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    tree: Arc<ProcessTree>,
}

#[cfg(unix)]
fn spawn_pty(cwd: &Path, cols: u16, rows: u16) -> Result<Spawned, TerminalError> {
    let system = native_pty_system();
    let pair = system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|err| TerminalError::Spawn(err.to_string()))?;
    let mut cmd = CommandBuilder::new(user_shell());
    cmd.cwd(cwd);
    for (key, value) in embedded_terminal_env() {
        cmd.env(key, value);
    }
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|err| TerminalError::Spawn(err.to_string()))?;
    let setup = (|| {
        let tree = ProcessTree::new(
            child
                .process_id()
                .ok_or_else(|| std::io::Error::other("terminal process id unavailable"))?,
        )?;
        let fd = pair
            .master
            .as_raw_fd()
            .ok_or_else(|| std::io::Error::other("terminal reader handle unavailable"))?;
        let reader = tree.reader(fd)?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok::<_, std::io::Error>((tree, reader, writer))
    })();
    let (tree, reader, writer) = match setup {
        Ok(parts) => parts,
        Err(error) => {
            if let Some(pid) = child.process_id() {
                if let Ok(tree) = ProcessTree::new(pid) {
                    let _ = tree.terminate_and_wait(Instant::now() + TERMINAL_EXIT_GRACE);
                }
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(TerminalError::Spawn(error.to_string()));
        }
    };
    Ok(Spawned {
        master: pair.master,
        writer,
        reader,
        child,
        tree,
    })
}

#[cfg(windows)]
fn spawn_pty(cwd: &Path, cols: u16, rows: u16) -> Result<Spawned, TerminalError> {
    let spawned = windows::spawn(&user_shell(), cwd, cols, rows, &embedded_terminal_env())
        .map_err(|error| TerminalError::Spawn(error.to_string()))?;
    Ok(Spawned {
        master: spawned.master,
        writer: spawned.writer,
        reader: spawned.reader,
        child: spawned.child,
        tree: spawned.tree,
    })
}

/// The terminal type the desktop renders: xterm.js speaks xterm-256color.
const EMBEDDED_TERM: &str = "xterm-256color";

/// Environment the embedded shell needs regardless of how the app itself was
/// launched.
///
/// A desktop app started from Finder inherits no `TERM`, and portable-pty
/// sets none. Without it, a line editor has no terminfo: zsh cannot move the
/// cursor left, so an erase redraws forward and leaves the deleted character
/// plus a space on screen. The renderer is always xterm.js, so the values are
/// fixed rather than inherited from whatever terminal launched the app, and
/// `TERM_PROGRAM` names this app the way other terminal emulators name
/// themselves.
fn embedded_terminal_env() -> [(&'static str, &'static str); 4] {
    [
        ("TERM", EMBEDDED_TERM),
        ("COLORTERM", "truecolor"),
        ("TERM_PROGRAM", "tidebreak"),
        ("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION")),
    ]
}

fn user_shell() -> PathBuf {
    if let Ok(shell) = std::env::var("SHELL") {
        if !shell.is_empty() {
            return PathBuf::from(shell);
        }
    }
    #[cfg(windows)]
    {
        PathBuf::from("powershell.exe")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/bin/sh")
    }
}

fn start_reader(
    handle: Arc<Mutex<LiveTerminal>>,
    notices: broadcast::Sender<TerminalNotice>,
    mut reader: Box<dyn Read + Send>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("code-terminal-read".into())
        .spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut live = handle.lock().expect("terminal");
                        // Keep these bytes even once the reaper has flipped
                        // `ended`. The shell wrote them before it exited, and
                        // dropping them loses the tail of the client's own
                        // command — the last thing it wants to read.
                        live.ring.write(&buf[..n]);
                        if !live.producing {
                            // Closed from the hub. Stop rather than fill a ring
                            // no reader can reach.
                            break;
                        }
                        let workspace_id = live.workspace_id;
                        let id = live.id;
                        apply_coalesce(&mut live, &handle, &notices, workspace_id, id);
                    }
                    Err(_) => break,
                }
            }
            drop(reader);
            if let Ok(mut live) = handle.lock() {
                live.reader_exit.mark_done(true);
                live.ended = true;
                live.producing = false;
                let workspace_id = live.workspace_id;
                let id = live.id;
                drop(live);
                publish_notice(&notices, workspace_id, id);
            }
        })
        .expect("code-terminal-read thread")
}

fn start_reaper(
    handle: Arc<Mutex<LiveTerminal>>,
    notices: broadcast::Sender<TerminalNotice>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    exit: Arc<TerminalExit>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("code-terminal-wait".into())
        .spawn(move || {
            #[cfg(unix)]
            let succeeded = {
                let tree = handle.lock().expect("terminal").tree.clone().expect("process tree");
                // Keep the unreaped shell as the session identity until every
                // descendant stops. A failed cleanup must retain that anchor.
                loop {
                    match tree.wait_and_reap(child.as_mut()) {
                        Ok(_) => break true,
                        Err(error) => {
                            tracing::warn!(%error, "terminal process cleanup failed; retaining session ownership");
                            thread::sleep(Duration::from_secs(1));
                        }
                    }
                }
            };
            #[cfg(windows)]
            let succeeded = child.wait().is_ok();
            drop(child);
            exit.mark_done(succeeded);
            if let Ok(mut live) = handle.lock() {
                if !live.ended {
                    // The shell is gone, so nothing can be typed at it. Leave
                    // `producing` to the reader: the PTY still holds whatever
                    // the shell wrote on its way out, and the terminal is not
                    // finished until that has landed in the ring.
                    live.ended = true;
                    live.writer = None;
                    let workspace_id = live.workspace_id;
                    let id = live.id;
                    drop(live);
                    publish_notice(&notices, workspace_id, id);
                }
            }
        })
        .expect("code-terminal-wait thread")
}

fn publish_notice(
    notices: &broadcast::Sender<TerminalNotice>,
    workspace_id: WorkspaceId,
    terminal_id: CodeTerminalId,
) {
    let _ = notices.send(TerminalNotice {
        workspace_id,
        terminal_id,
    });
}

struct PendingNotice {
    due: Instant,
    handle: Weak<Mutex<LiveTerminal>>,
    notices: broadcast::Sender<TerminalNotice>,
    workspace_id: WorkspaceId,
    terminal_id: CodeTerminalId,
}

fn schedule_trailing_notice(
    handle: Arc<Mutex<LiveTerminal>>,
    notices: broadcast::Sender<TerminalNotice>,
    workspace_id: WorkspaceId,
    terminal_id: CodeTerminalId,
) {
    static SCHEDULER: std::sync::OnceLock<std::sync::mpsc::Sender<PendingNotice>> =
        std::sync::OnceLock::new();
    let scheduler = SCHEDULER.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::channel::<PendingNotice>();
        thread::Builder::new()
            .name("code-terminal-notice".into())
            .spawn(move || {
                let mut pending = Vec::<PendingNotice>::new();
                loop {
                    let received = match pending.iter().map(|notice| notice.due).min() {
                        Some(due) => {
                            receiver.recv_timeout(due.saturating_duration_since(Instant::now()))
                        }
                        None => receiver
                            .recv()
                            .map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected),
                    };
                    match received {
                        Ok(notice) => pending.push(notice),
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    let now = Instant::now();
                    pending.retain(|notice| {
                        if notice.due > now {
                            return true;
                        }
                        if let Some(handle) = notice.handle.upgrade() {
                            if let Ok(mut live) = handle.lock() {
                                let dirty = live.coalesce.dirty;
                                live.coalesce.dirty = false;
                                live.coalesce.scheduled = false;
                                if dirty {
                                    live.coalesce.quiet_until = now + TERMINAL_NOTICE_COALESCE;
                                    drop(live);
                                    publish_notice(
                                        &notice.notices,
                                        notice.workspace_id,
                                        notice.terminal_id,
                                    );
                                }
                            }
                        }
                        false
                    });
                }
            })
            .expect("code-terminal-notice thread");
        sender
    });
    let _ = scheduler.send(PendingNotice {
        due: Instant::now() + TERMINAL_NOTICE_COALESCE,
        handle: Arc::downgrade(&handle),
        notices,
        workspace_id,
        terminal_id,
    });
}

fn apply_coalesce(
    live: &mut LiveTerminal,
    handle: &Arc<Mutex<LiveTerminal>>,
    notices: &broadcast::Sender<TerminalNotice>,
    workspace_id: WorkspaceId,
    terminal_id: CodeTerminalId,
) {
    match live.coalesce.mark() {
        CoalesceAction::PublishNow => publish_notice(notices, workspace_id, terminal_id),
        CoalesceAction::Schedule => schedule_trailing_notice(
            Arc::clone(handle),
            notices.clone(),
            workspace_id,
            terminal_id,
        ),
        CoalesceAction::Wait => {}
    }
}

enum CoalesceAction {
    PublishNow,
    Schedule,
    Wait,
}

impl Coalesce {
    fn new() -> Self {
        Self {
            dirty: false,
            scheduled: false,
            quiet_until: Instant::now(),
        }
    }

    fn mark(&mut self) -> CoalesceAction {
        let now = Instant::now();
        if now >= self.quiet_until {
            self.dirty = false;
            self.scheduled = false;
            self.quiet_until = now + TERMINAL_NOTICE_COALESCE;
            CoalesceAction::PublishNow
        } else if self.scheduled {
            self.dirty = true;
            CoalesceAction::Wait
        } else {
            self.dirty = true;
            self.scheduled = true;
            CoalesceAction::Schedule
        }
    }
}

fn clamp_size(value: u16) -> Result<u16, TerminalError> {
    if (MIN_SIZE..=MAX_SIZE).contains(&value) {
        Ok(value)
    } else {
        Err(TerminalError::InvalidSize)
    }
}

fn lock_snapshot(handle: &Arc<Mutex<LiveTerminal>>) -> TerminalSnapshot {
    snapshot(&handle.lock().expect("terminal"))
}

fn snapshot(live: &LiveTerminal) -> TerminalSnapshot {
    TerminalSnapshot {
        id: live.id,
        workspace_id: live.workspace_id,
        cols: live.cols,
        rows: live.rows,
        ended: live.ended,
        created_at: live.created_at,
    }
}

/// Bounded circular buffer addressed by a monotonic byte cursor.
struct ByteRing {
    buf: Vec<u8>,
    cap: usize,
    start: u64,
    len: usize,
    head: usize,
}

impl ByteRing {
    fn new(cap: usize) -> Self {
        Self {
            buf: vec![0; cap],
            cap,
            start: 0,
            len: 0,
            head: 0,
        }
    }

    fn end(&self) -> u64 {
        self.start + self.len as u64
    }

    fn write(&mut self, bytes: &[u8]) {
        if bytes.len() >= self.cap {
            self.start = self.end() + bytes.len() as u64 - self.cap as u64;
            self.buf.copy_from_slice(&bytes[bytes.len() - self.cap..]);
            self.head = 0;
            self.len = self.cap;
            return;
        }
        let discard = (self.len + bytes.len()).saturating_sub(self.cap);
        self.start += discard as u64;
        self.head = (self.head + discard) % self.cap;
        self.len -= discard;
        let tail = (self.head + self.len) % self.cap;
        let first = bytes.len().min(self.cap - tail);
        self.buf[tail..tail + first].copy_from_slice(&bytes[..first]);
        self.buf[..bytes.len() - first].copy_from_slice(&bytes[first..]);
        self.len += bytes.len();
    }

    fn read(&self, cursor: u64, max: usize) -> (Vec<u8>, u64, bool, bool) {
        let overflow = cursor < self.start;
        let pos = if overflow {
            self.start
        } else if cursor > self.end() {
            self.end()
        } else {
            cursor
        };
        let available = (self.end() - pos) as usize;
        let take = available.min(max);
        let mut out = Vec::with_capacity(take + if overflow { TRUNCATION_MARKER.len() } else { 0 });
        if overflow {
            out.extend_from_slice(TRUNCATION_MARKER);
        }
        if take > 0 {
            let offset = (pos - self.start) as usize;
            let first_index = (self.head + offset) % self.cap;
            let first = take.min(self.cap - first_index);
            out.extend_from_slice(&self.buf[first_index..first_index + first]);
            out.extend_from_slice(&self.buf[..take - first]);
        }
        (out, pos + take as u64, overflow, take < available)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservations_enforce_the_cap_before_any_spawn_finishes() {
        let hub = TerminalHub::new();
        let workspace = workspace();
        let reservations = (0..MAX_TERMINALS_PER_WORKSPACE)
            .map(|_| hub.reserve_slot(workspace).unwrap())
            .collect::<Vec<_>>();
        assert!(matches!(
            hub.reserve_slot(workspace),
            Err(TerminalError::WorkspaceCap)
        ));
        drop(reservations);
        assert!(hub.reserve_slot(workspace).is_ok());
    }

    #[tokio::test]
    async fn failed_shutdown_stays_registered_for_the_next_archive_attempt() {
        let hub = TerminalHub::new();
        let workspace = workspace();
        let snapshot = hub
            .open_memory(&OwnerId::local(), workspace, 80, 24)
            .unwrap();
        let handle = handle_of(&hub, snapshot.id);
        handle.lock().unwrap().exit = Arc::new(TerminalExit::new(false));
        handle.lock().unwrap().exit.mark_done(false);
        assert!(!hub.close_workspace_and_wait(workspace).await);
        assert!(hub.get(workspace, snapshot.id).is_some());
        handle.lock().unwrap().exit.mark_done(true);
        assert!(hub.close_workspace_and_wait(workspace).await);
        assert!(hub.get(workspace, snapshot.id).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn closing_a_terminal_stops_background_jobs_and_joins_the_reader() {
        let root = tempfile::tempdir().unwrap();
        let hub = TerminalHub::new();
        let workspace = workspace();
        let snapshot = hub
            .open(&OwnerId::local(), workspace, root.path(), None, None)
            .unwrap();
        let handle = handle_of(&hub, snapshot.id);
        hub.write(
            workspace,
            snapshot.id,
            b"/bin/sh -c 'trap \"\" HUP; sleep 60 & echo $! > terminal-child.pid; wait' &\n",
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !root.path().join("terminal-child.pid").exists() {
            assert!(Instant::now() < deadline, "terminal job never started");
            thread::sleep(Duration::from_millis(10));
        }
        hub.close(workspace, snapshot.id).unwrap();
        let live = handle.lock().unwrap();
        assert!(!live.producing);
        assert!(live.reader_thread.is_none());
        assert!(live.reaper_thread.is_none());
        assert!(live.master.is_none());
        assert!(live
            .tree
            .as_ref()
            .unwrap()
            .terminate_and_wait(Instant::now())
            .unwrap());
    }

    fn workspace() -> WorkspaceId {
        WorkspaceId::new()
    }

    fn handle_of(hub: &TerminalHub, id: CodeTerminalId) -> Arc<Mutex<LiveTerminal>> {
        hub.inner
            .lock()
            .expect("terminal hub")
            .by_id
            .get(&id)
            .cloned()
            .expect("terminal")
    }

    /// A PTY-shaped source the test drives one chunk at a time. Dropping the
    /// sender is the EOF a closed slave produces.
    struct ScriptedReader {
        chunks: std::sync::mpsc::Receiver<Vec<u8>>,
    }

    impl Read for ScriptedReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.chunks.recv() {
                Ok(chunk) => {
                    let n = chunk.len().min(buf.len());
                    buf[..n].copy_from_slice(&chunk[..n]);
                    Ok(n)
                }
                Err(_) => Ok(0),
            }
        }
    }

    /// The reaper flips `ended` when the child exits, which happens before the
    /// reader has drained what the shell wrote on its way out. The tail of the
    /// client's own command is the last thing it wants to lose.
    #[test]
    fn reader_keeps_output_the_shell_wrote_before_it_was_reaped() {
        let hub = TerminalHub::new();
        let ws = workspace();
        let snap = hub.open_memory(&OwnerId::local(), ws, 80, 24).unwrap();
        let handle = handle_of(&hub, snap.id);
        handle.lock().unwrap().producing = true;

        let (tx, rx) = std::sync::mpsc::channel();
        start_reader(
            handle.clone(),
            hub.notices_sender(&OwnerId::local()),
            Box::new(ScriptedReader { chunks: rx }),
        );

        // The shell exits with its last line still in the PTY.
        handle.lock().unwrap().ended = true;
        tx.send(b"tail of the command\r\n".to_vec()).unwrap();
        drop(tx);

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let read = hub.read(ws, snap.id, 0);
            if read.ended {
                assert_eq!(read.data, b"tail of the command\r\n");
                return;
            }
            assert!(Instant::now() < deadline, "the reader never drained");
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// `ended` is a client's signal to stop polling, so it has to mean every
    /// byte has landed, not just that the shell is gone.
    #[test]
    fn a_terminal_is_not_finished_while_its_reader_is_still_draining() {
        let hub = TerminalHub::new();
        let ws = workspace();
        let snap = hub.open_memory(&OwnerId::local(), ws, 80, 24).unwrap();
        let handle = handle_of(&hub, snap.id);
        handle.lock().unwrap().producing = true;

        hub.push_output(snap.id, b"before ");
        handle.lock().unwrap().ended = true;

        let mid = hub.read(ws, snap.id, 0);
        assert_eq!(mid.data, b"before ");
        assert!(!mid.ended, "bytes are still in flight");
        // The snapshot answers the other question — whether the shell is there
        // to type at — and that one is already settled.
        assert!(hub.get(ws, snap.id).unwrap().ended);

        hub.push_output(snap.id, b"after");
        handle.lock().unwrap().producing = false;

        let end = hub.read(ws, snap.id, mid.next_cursor);
        assert_eq!(end.data, b"after");
        assert!(end.ended);
    }

    #[test]
    fn ring_wraparound_drops_oldest_and_advances_start() {
        let mut ring = ByteRing::new(8);
        ring.write(b"abcdefgh");
        assert_eq!(ring.start, 0);
        assert_eq!(ring.end(), 8);
        ring.write(b"ij");
        assert_eq!(ring.start, 2);
        assert_eq!(ring.end(), 10);
        let (data, next, overflow, truncated) = ring.read(2, 32);
        assert!(!overflow);
        assert!(!truncated);
        assert_eq!(data, b"cdefghij");
        assert_eq!(next, 10);
    }

    #[test]
    fn cursor_at_overflow_boundary_is_not_stale() {
        let mut ring = ByteRing::new(4);
        ring.write(b"abcdef");
        // start is now 2; cursor == start is the first retained byte.
        let (data, next, overflow, _) = ring.read(2, 32);
        assert!(!overflow);
        assert_eq!(data, b"cdef");
        assert_eq!(next, 6);
    }

    #[test]
    fn stale_cursor_gets_inline_truncation_marker() {
        let mut ring = ByteRing::new(4);
        ring.write(b"abcdefgh");
        let (data, next, overflow, _) = ring.read(0, 32);
        assert!(overflow);
        assert!(data.starts_with(TRUNCATION_MARKER));
        assert_eq!(&data[TRUNCATION_MARKER.len()..], b"efgh");
        assert_eq!(next, 8);
    }

    #[test]
    fn read_is_capped() {
        let mut ring = ByteRing::new(64);
        ring.write(&[b'x'; 40]);
        let (data, next, overflow, truncated) = ring.read(0, 16);
        assert!(!overflow);
        assert!(truncated);
        assert_eq!(data.len(), 16);
        assert_eq!(next, 16);
        let (rest, end, _, truncated_rest) = ring.read(next, 16);
        assert!(truncated_rest);
        assert_eq!(rest.len(), 16);
        assert_eq!(end, 32);
        let (tail, last, _, last_trunc) = ring.read(end, 16);
        assert!(!last_trunc);
        assert_eq!(tail.len(), 8);
        assert_eq!(last, 40);
    }

    #[test]
    fn two_readers_at_different_cursors_both_see_retained_bytes() {
        let hub = TerminalHub::new();
        let ws = workspace();
        let snap = hub.open_memory(&OwnerId::local(), ws, 80, 24).unwrap();
        hub.push_output(snap.id, b"hello ");
        hub.push_output(snap.id, b"world");
        let first = hub.read(ws, snap.id, 0);
        assert_eq!(first.data, b"hello world");
        let mid = hub.read(ws, snap.id, 6);
        assert_eq!(mid.data, b"world");
        assert_eq!(mid.next_cursor, first.next_cursor);
    }

    #[test]
    fn fast_producer_coalesces_notices_and_reader_gets_every_byte() {
        let hub = TerminalHub::new();
        let ws = workspace();
        let snap = hub.open_memory(&OwnerId::local(), ws, 80, 24).unwrap();
        let mut rx = hub.subscribe(&OwnerId::local());
        let mut expected = Vec::new();
        for i in 0..64u8 {
            let chunk = [i];
            expected.extend_from_slice(&chunk);
            hub.push_output(snap.id, &chunk);
        }
        thread::sleep(TERMINAL_NOTICE_COALESCE + Duration::from_millis(20));
        let mut notices = 0;
        while rx.try_recv().is_ok() {
            notices += 1;
        }
        assert!(
            notices > 0 && notices <= 4,
            "expected coalesced notices, got {notices}"
        );
        let read = hub.read(ws, snap.id, 0);
        assert_eq!(read.data, expected);
        assert!(!read.overflow);
        assert!(!read.truncated);
    }

    // The probe is a POSIX `printf`, and `user_shell()` falls back to
    // PowerShell on Windows, which never expands it.

    #[test]
    fn restart_reaps_terminals_and_leaves_no_durable_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let marker = b"TERM_MARKER_no_durable_9f3a7c1e";
        let ws = workspace();
        let first = TerminalHub::new();
        let snap = first.open_memory(&OwnerId::local(), ws, 80, 24).unwrap();
        first.push_output(snap.id, marker);
        let before = first.read(ws, snap.id, 0);
        assert!(before.data.windows(marker.len()).any(|w| w == marker));
        drop(first);

        let second = TerminalHub::new();
        assert!(second.list(ws).is_empty());
        let read = second.read(ws, snap.id, 0);
        assert!(read.ended);
        assert!(read.data.is_empty());
        assert!(second.get(ws, snap.id).is_none());
        assert_no_bytes(dir.path(), marker);
    }

    #[test]
    fn write_size_and_workspace_caps() {
        let hub = TerminalHub::new();
        let ws = workspace();
        let snap = hub.open_memory(&OwnerId::local(), ws, 80, 24).unwrap();
        let too_big = vec![b'a'; MAX_TERMINAL_WRITE_BYTES + 1];
        assert!(matches!(
            hub.write(ws, snap.id, &too_big),
            Err(TerminalError::WriteTooLarge)
        ));
        for _ in 1..MAX_TERMINALS_PER_WORKSPACE {
            hub.open_memory(&OwnerId::local(), ws, 80, 24).unwrap();
        }
        assert!(matches!(
            hub.open_memory(&OwnerId::local(), ws, 80, 24),
            Err(TerminalError::WorkspaceCap)
        ));
    }

    fn assert_no_bytes(root: &Path, needle: &[u8]) {
        fn walk(path: &Path, needle: &[u8]) {
            let Ok(meta) = std::fs::metadata(path) else {
                return;
            };
            if meta.is_file() {
                let Ok(bytes) = std::fs::read(path) else {
                    return;
                };
                assert!(
                    !bytes.windows(needle.len()).any(|window| window == needle),
                    "terminal bytes persisted at {}",
                    path.display()
                );
            } else if meta.is_dir() {
                let Ok(entries) = std::fs::read_dir(path) else {
                    return;
                };
                for entry in entries.flatten() {
                    walk(&entry.path(), needle);
                }
            }
        }
        walk(root, needle);
    }
}
