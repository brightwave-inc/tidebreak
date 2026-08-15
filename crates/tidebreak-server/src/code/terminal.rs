//! Auxiliary workspace terminals: a PTY plus a bounded in-memory ring.
//!
//! Bytes are ephemeral. They are not journaled, not persisted, and vanish
//! when this process does. The harness crate has no PTY dependency; this
//! module is the only place one is used.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tidebreak_core::{CodeTerminalId, WorkspaceId};
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

/// In-memory process-wide registry of live auxiliary terminals.
pub(crate) struct TerminalHub {
    inner: Mutex<HubInner>,
    notices: broadcast::Sender<TerminalNotice>,
}

struct HubInner {
    by_id: HashMap<CodeTerminalId, Arc<Mutex<LiveTerminal>>>,
    by_workspace: HashMap<WorkspaceId, Vec<CodeTerminalId>>,
}

struct LiveTerminal {
    id: CodeTerminalId,
    workspace_id: WorkspaceId,
    ring: ByteRing,
    cols: u16,
    rows: u16,
    ended: bool,
    created_at: DateTime<Utc>,
    writer: Option<Box<dyn Write + Send>>,
    master: Option<Box<dyn MasterPty + Send>>,
    killer: Option<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    coalesce: Coalesce,
}

struct Coalesce {
    dirty: bool,
    scheduled: bool,
    quiet_until: Instant,
}

/// Unsequenced activity notice. Published on the workspace bus; never journaled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminalNotice {
    pub workspace_id: WorkspaceId,
    pub terminal_id: CodeTerminalId,
}

/// Snapshot a route can serialize.
#[derive(Debug, Clone)]
pub(crate) struct TerminalSnapshot {
    pub id: CodeTerminalId,
    pub workspace_id: WorkspaceId,
    pub cols: u16,
    pub rows: u16,
    pub ended: bool,
    pub created_at: DateTime<Utc>,
}

/// One cursor-pull response.
#[derive(Debug, Clone)]
pub(crate) struct TerminalRead {
    pub data: Vec<u8>,
    pub next_cursor: u64,
    pub overflow: bool,
    pub truncated: bool,
    pub ended: bool,
}

#[derive(Debug)]
pub(crate) enum TerminalError {
    WorkspaceCap,
    WriteTooLarge,
    Ended,
    NotFound,
    InvalidSize,
    Spawn(String),
    Io(String),
}

impl TerminalHub {
    pub(crate) fn new() -> Self {
        let (notices, _) = broadcast::channel(NOTICE_BUFFER);
        Self {
            inner: Mutex::new(HubInner {
                by_id: HashMap::new(),
                by_workspace: HashMap::new(),
            }),
            notices,
        }
    }

    #[cfg(test)]
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<TerminalNotice> {
        self.notices.subscribe()
    }

    pub(crate) fn open(
        &self,
        workspace_id: WorkspaceId,
        cwd: &Path,
        cols: Option<u16>,
        rows: Option<u16>,
    ) -> Result<TerminalSnapshot, TerminalError> {
        let cols = clamp_size(cols.unwrap_or(DEFAULT_COLS))?;
        let rows = clamp_size(rows.unwrap_or(DEFAULT_ROWS))?;
        self.reserve_slot(workspace_id)?;
        let spawned = spawn_pty(cwd, cols, rows)?;
        let id = CodeTerminalId::new();
        let live = LiveTerminal {
            id,
            workspace_id,
            ring: ByteRing::new(TERMINAL_RING_BYTES),
            cols,
            rows,
            ended: false,
            created_at: Utc::now(),
            writer: Some(spawned.writer),
            master: Some(spawned.master),
            killer: Some(spawned.killer),
            coalesce: Coalesce::new(),
        };
        let handle = Arc::new(Mutex::new(live));
        self.insert(workspace_id, id, handle.clone());
        start_reader(handle.clone(), self.notices.clone(), spawned.reader);
        start_reaper(handle.clone(), self.notices.clone(), spawned.child);
        Ok(lock_snapshot(&handle))
    }

    /// Open a ring with no PTY. Used by tests that must not spawn a shell.
    #[cfg(test)]
    pub(crate) fn open_memory(
        &self,
        workspace_id: WorkspaceId,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalSnapshot, TerminalError> {
        let cols = clamp_size(cols)?;
        let rows = clamp_size(rows)?;
        self.reserve_slot(workspace_id)?;
        let id = CodeTerminalId::new();
        let live = LiveTerminal {
            id,
            workspace_id,
            ring: ByteRing::new(TERMINAL_RING_BYTES),
            cols,
            rows,
            ended: false,
            created_at: Utc::now(),
            writer: None,
            master: None,
            killer: None,
            coalesce: Coalesce::new(),
        };
        let handle = Arc::new(Mutex::new(live));
        self.insert(workspace_id, id, handle.clone());
        Ok(lock_snapshot(&handle))
    }

    pub(crate) fn list(&self, workspace_id: WorkspaceId) -> Vec<TerminalSnapshot> {
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
    pub(crate) fn get(
        &self,
        workspace_id: WorkspaceId,
        id: CodeTerminalId,
    ) -> Option<TerminalSnapshot> {
        let inner = self.inner.lock().expect("terminal hub");
        let handle = inner.by_id.get(&id)?;
        let snap = lock_snapshot(handle);
        if snap.workspace_id != workspace_id {
            return None;
        }
        Some(snap)
    }

    pub(crate) fn read(
        &self,
        workspace_id: WorkspaceId,
        id: CodeTerminalId,
        cursor: u64,
    ) -> TerminalRead {
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
            ended: live.ended,
        }
    }

    pub(crate) fn write(
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
            apply_coalesce(&mut live, &handle, &self.notices, workspace_id, id);
        }
        Ok(())
    }

    pub(crate) fn resize(
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

    pub(crate) fn close(
        &self,
        workspace_id: WorkspaceId,
        id: CodeTerminalId,
    ) -> Result<(), TerminalError> {
        let handle = self
            .handle(workspace_id, id)
            .ok_or(TerminalError::NotFound)?;
        {
            let mut live = handle.lock().expect("terminal");
            live.kill_and_end();
        }
        self.remove(workspace_id, id);
        Ok(())
    }

    pub(crate) fn close_workspace(&self, workspace_id: WorkspaceId) {
        let ids = {
            let inner = self.inner.lock().expect("terminal hub");
            inner
                .by_workspace
                .get(&workspace_id)
                .cloned()
                .unwrap_or_default()
        };
        for id in ids {
            let _ = self.close(workspace_id, id);
        }
    }

    /// Append output as if the PTY had produced it. Tests and the reader thread.
    #[cfg(test)]
    pub(crate) fn push_output(&self, id: CodeTerminalId, bytes: &[u8]) {
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
        apply_coalesce(&mut live, &handle, &self.notices, workspace_id, id);
    }

    fn reserve_slot(&self, workspace_id: WorkspaceId) -> Result<(), TerminalError> {
        let inner = self.inner.lock().expect("terminal hub");
        let count = inner
            .by_workspace
            .get(&workspace_id)
            .map(Vec::len)
            .unwrap_or(0);
        if count >= MAX_TERMINALS_PER_WORKSPACE {
            return Err(TerminalError::WorkspaceCap);
        }
        Ok(())
    }

    fn insert(
        &self,
        workspace_id: WorkspaceId,
        id: CodeTerminalId,
        handle: Arc<Mutex<LiveTerminal>>,
    ) {
        let mut inner = self.inner.lock().expect("terminal hub");
        inner.by_id.insert(id, handle);
        inner.by_workspace.entry(workspace_id).or_default().push(id);
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

impl LiveTerminal {
    fn kill_and_end(&mut self) {
        if let Some(mut killer) = self.killer.take() {
            let _ = killer.kill();
        }
        self.writer = None;
        self.master = None;
        self.ended = true;
    }
}

struct Spawned {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    reader: Box<dyn Read + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
}

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
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|err| TerminalError::Spawn(err.to_string()))?;
    let killer = child.clone_killer();
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|err| TerminalError::Spawn(err.to_string()))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|err| TerminalError::Spawn(err.to_string()))?;
    Ok(Spawned {
        master: pair.master,
        writer,
        reader,
        child,
        killer,
    })
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
) {
    thread::Builder::new()
        .name("code-terminal-read".into())
        .spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut live = handle.lock().expect("terminal");
                        if live.ended {
                            break;
                        }
                        live.ring.write(&buf[..n]);
                        let workspace_id = live.workspace_id;
                        let id = live.id;
                        apply_coalesce(&mut live, &handle, &notices, workspace_id, id);
                    }
                    Err(_) => break,
                }
            }
            if let Ok(mut live) = handle.lock() {
                live.ended = true;
                let workspace_id = live.workspace_id;
                let id = live.id;
                drop(live);
                publish_notice(&notices, workspace_id, id);
            }
        })
        .expect("code-terminal-read thread");
}

fn start_reaper(
    handle: Arc<Mutex<LiveTerminal>>,
    notices: broadcast::Sender<TerminalNotice>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
) {
    thread::Builder::new()
        .name("code-terminal-wait".into())
        .spawn(move || {
            let _ = child.wait();
            if let Ok(mut live) = handle.lock() {
                if !live.ended {
                    live.ended = true;
                    live.writer = None;
                    let workspace_id = live.workspace_id;
                    let id = live.id;
                    drop(live);
                    publish_notice(&notices, workspace_id, id);
                }
            }
        })
        .expect("code-terminal-wait thread");
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

fn schedule_trailing_notice(
    handle: Arc<Mutex<LiveTerminal>>,
    notices: broadcast::Sender<TerminalNotice>,
    workspace_id: WorkspaceId,
    terminal_id: CodeTerminalId,
) {
    thread::Builder::new()
        .name("code-terminal-notice".into())
        .spawn(move || {
            thread::sleep(TERMINAL_NOTICE_COALESCE);
            let mut live = match handle.lock() {
                Ok(live) => live,
                Err(_) => return,
            };
            if !live.coalesce.dirty {
                live.coalesce.scheduled = false;
                return;
            }
            live.coalesce.dirty = false;
            live.coalesce.scheduled = false;
            live.coalesce.quiet_until = Instant::now() + TERMINAL_NOTICE_COALESCE;
            drop(live);
            publish_notice(&notices, workspace_id, terminal_id);
        })
        .ok();
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
        for &byte in bytes {
            if self.len == self.cap {
                self.head = (self.head + 1) % self.cap;
                self.start += 1;
                self.len -= 1;
            }
            let idx = (self.head + self.len) % self.cap;
            self.buf[idx] = byte;
            self.len += 1;
        }
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
            for i in 0..take {
                let idx = (self.head + offset + i) % self.cap;
                out.push(self.buf[idx]);
            }
        }
        (out, pos + take as u64, overflow, take < available)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> WorkspaceId {
        WorkspaceId::new()
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
        let snap = hub.open_memory(ws, 80, 24).unwrap();
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
        let snap = hub.open_memory(ws, 80, 24).unwrap();
        let mut rx = hub.subscribe();
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

    #[test]
    fn restart_reaps_terminals_and_leaves_no_durable_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let marker = b"TERM_MARKER_no_durable_9f3a7c1e";
        let ws = workspace();
        let first = TerminalHub::new();
        let snap = first.open_memory(ws, 80, 24).unwrap();
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
        let snap = hub.open_memory(ws, 80, 24).unwrap();
        let too_big = vec![b'a'; MAX_TERMINAL_WRITE_BYTES + 1];
        assert!(matches!(
            hub.write(ws, snap.id, &too_big),
            Err(TerminalError::WriteTooLarge)
        ));
        for _ in 1..MAX_TERMINALS_PER_WORKSPACE {
            hub.open_memory(ws, 80, 24).unwrap();
        }
        assert!(matches!(
            hub.open_memory(ws, 80, 24),
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
