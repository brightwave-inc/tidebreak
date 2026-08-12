//! Process-wide tracing subscriber for the desktop shell and `tidebreak serve`.
//!
//! Events land in a bounded log file under the profile data directory —
//! `logs/tidebreak.log` next to `tidebreak.db` — so a warning like a gateway
//! mount failure leaves a diagnosable trace on the user's machine. When the
//! file passes [`LOG_ROTATE_BYTES`] it is renamed to `tidebreak.log.1`
//! (replacing any previous rotation) and a fresh file is started, capping the
//! profile's log footprint at roughly two files of that size. Debug builds
//! also mirror events to stderr for `tidebreak serve` / `tauri dev` terminals.
//!
//! The default level policy is `info` for the workspace's own `tidebreak*`
//! crates and `warn` for everything else. The `TIDEBREAK_LOG` environment
//! variable overrides it with standard `tracing_subscriber::EnvFilter`
//! directives (e.g. `TIDEBREAK_LOG=debug` or
//! `TIDEBREAK_LOG=warn,tidebreak_server=trace`); an invalid spec falls back to
//! the default rather than failing boot.
//!
//! # Redaction posture
//!
//! The workspace's emit sites are disciplined: reqwest errors are URL-stripped
//! before logging, policy diagnostics name environment *variables* rather than
//! values, and no secret or token ever appears in an error's `Display` text.
//! This subscriber must not widen that surface: it uses the plain compact
//! formatter (timestamp, level, target, message) and records no span-lifecycle
//! events, so nothing beyond the literal event fields reaches disk. New log
//! sites must uphold the same posture — log names and shapes, never values
//! that could carry a credential, URL query, or file contents.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Rotate `tidebreak.log` to `tidebreak.log.1` once it would pass this size.
pub const LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;

/// Environment variable holding `EnvFilter` directives that override
/// [`DEFAULT_DIRECTIVES`].
const LOG_ENV_VAR: &str = "TIDEBREAK_LOG";

/// `info` for the workspace's own crates, `warn` for dependencies.
const DEFAULT_DIRECTIVES: &str = "warn,tidebreak_cli=info,tidebreak_code_execution=info,\
    tidebreak_core=info,tidebreak_desktop=info,tidebreak_egress=info,\
    tidebreak_host_broker=info,tidebreak_mcp=info,tidebreak_router=info,\
    tidebreak_sandbox_agent=info,tidebreak_sandbox_protocol=info,tidebreak_server=info,\
    tidebreak_shell_policy=info";

/// Install the process-global subscriber, writing to `logs/tidebreak.log`
/// under `data_dir`.
///
/// Infallible by design: if the log directory or file cannot be created the
/// subscriber degrades to stderr-only, and if a subscriber is already
/// installed (tests, a second call) the call is a no-op. Logging must never
/// block boot.
pub fn init_logging(data_dir: &Path) {
    let filter = env_filter();
    match open_log_writer(data_dir) {
        Ok(writer) => {
            let subscriber = tracing_subscriber::registry().with(filter).with(
                tracing_subscriber::fmt::layer()
                    .compact()
                    .with_ansi(false)
                    .with_writer(writer),
            );
            #[cfg(debug_assertions)]
            let subscriber = subscriber.with(
                tracing_subscriber::fmt::layer()
                    .compact()
                    .with_ansi(false)
                    .with_writer(io::stderr),
            );
            let _ = subscriber.try_init();
        }
        Err(error) => {
            eprintln!("tidebreak: profile log file unavailable ({error}); logging to stderr only");
            let subscriber = tracing_subscriber::registry().with(filter).with(
                tracing_subscriber::fmt::layer()
                    .compact()
                    .with_ansi(false)
                    .with_writer(io::stderr),
            );
            let _ = subscriber.try_init();
        }
    }
}

/// Install the file-only subscriber used by `tidebreak tui`.
///
/// The TUI owns the terminal, so unlike [`init_logging`] there is no stderr
/// mirror — and an unusable log file degrades to no subscriber at all rather
/// than stderr, which would corrupt the display.
pub fn init_logging_file_only(data_dir: &Path) {
    if let Ok(writer) = open_log_writer(data_dir) {
        let subscriber = tracing_subscriber::registry().with(env_filter()).with(
            tracing_subscriber::fmt::layer()
                .compact()
                .with_ansi(false)
                .with_writer(writer),
        );
        let _ = subscriber.try_init();
    }
}

/// The active filter: `TIDEBREAK_LOG` when set and valid, the default policy
/// otherwise.
fn env_filter() -> EnvFilter {
    match std::env::var(LOG_ENV_VAR) {
        Ok(spec) if !spec.trim().is_empty() => EnvFilter::try_new(&spec).unwrap_or_else(|error| {
            eprintln!("tidebreak: invalid {LOG_ENV_VAR} ({error}); using the default filter");
            EnvFilter::new(DEFAULT_DIRECTIVES)
        }),
        _ => EnvFilter::new(DEFAULT_DIRECTIVES),
    }
}

/// Create `logs/` under the data directory and open the bounded writer.
fn open_log_writer(data_dir: &Path) -> io::Result<RotatingFileWriter> {
    let logs = data_dir.join("logs");
    fs::create_dir_all(&logs)?;
    RotatingFileWriter::new(logs.join("tidebreak.log"), LOG_ROTATE_BYTES)
}

/// A size-capped log file with a single rotation slot.
///
/// Appends to `path`; once a write would push the file past `cap` bytes, the
/// file is renamed to `<path>.1` (replacing any previous rotation) and a
/// fresh file is started. Best-effort throughout: a failed rotation or reopen
/// silently drops output rather than surfacing errors into the subscriber.
#[derive(Clone)]
struct RotatingFileWriter {
    inner: Arc<Mutex<RotatingFile>>,
}

struct RotatingFile {
    path: PathBuf,
    cap: u64,
    /// `None` after a reopen failure; writes are then dropped.
    file: Option<File>,
    written: u64,
}

impl RotatingFileWriter {
    fn new(path: PathBuf, cap: u64) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata()?.len();
        Ok(Self {
            inner: Arc::new(Mutex::new(RotatingFile {
                path,
                cap,
                file: Some(file),
                written,
            })),
        })
    }
}

impl RotatingFile {
    /// Rename the current file to the `.1` slot and start a fresh one.
    ///
    /// The handle is dropped before the rename so Windows can move the file.
    fn rotate(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = (&file).flush();
            drop(file);
        }
        let mut rotated = self.path.clone().into_os_string();
        rotated.push(".1");
        let rotated = PathBuf::from(rotated);
        let _ = fs::remove_file(&rotated);
        let _ = fs::rename(&self.path, &rotated);
        self.file = File::create(&self.path).ok();
        self.written = 0;
    }

    fn write_all_bytes(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written > 0 && self.written.saturating_add(buf.len() as u64) > self.cap {
            self.rotate();
        }
        if let Some(file) = self.file.as_mut() {
            file.write_all(buf)?;
            self.written += buf.len() as u64;
        }
        // A dead writer (reopen failure) swallows output by design; reporting
        // an error here would only make the fmt layer spam stderr forever.
        Ok(buf.len())
    }
}

impl Write for RotatingFileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.inner.lock() {
            Ok(mut inner) => inner.write_all_bytes(buf),
            Err(_) => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Ok(mut inner) = self.inner.lock() {
            if let Some(file) = inner.file.as_mut() {
                file.flush()?;
            }
        }
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for RotatingFileWriter {
    type Writer = RotatingFileWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes past the cap rotate exactly once — old bytes move to the `.1`
    /// slot, new bytes keep landing in a fresh primary file.
    #[test]
    fn writes_past_the_cap_rotate_once_and_keep_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tidebreak.log");
        let mut writer = RotatingFileWriter::new(path.clone(), 64).unwrap();

        let first = [b'a'; 48];
        let second = [b'b'; 48];
        writer.write_all(&first).unwrap();
        // 48 + 48 > 64: this write rotates first.
        writer.write_all(&second).unwrap();
        // 48 + 10 <= 64: no second rotation.
        writer.write_all(b"tail-bytes").unwrap();
        writer.flush().unwrap();

        let rotated = fs::read(dir.path().join("tidebreak.log.1")).unwrap();
        assert_eq!(rotated, first);
        let current = fs::read(&path).unwrap();
        assert_eq!(
            current,
            b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbtail-bytes"
        );
    }

    /// A second rotation replaces the previous `.1` file instead of growing a
    /// chain, keeping the on-disk footprint bounded.
    #[test]
    fn a_later_rotation_replaces_the_previous_slot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tidebreak.log");
        let mut writer = RotatingFileWriter::new(path.clone(), 8).unwrap();

        writer.write_all(b"first").unwrap();
        writer.write_all(b"second").unwrap(); // rotates: .1 = "first"
        writer.write_all(b"third").unwrap(); // rotates: .1 = "second"
        writer.flush().unwrap();

        let rotated = fs::read(dir.path().join("tidebreak.log.1")).unwrap();
        assert_eq!(rotated, b"second");
        assert_eq!(fs::read(&path).unwrap(), b"third");
    }

    /// The degrade trigger: an unusable log location surfaces as an error from
    /// the writer constructor, which `init_logging` converts into stderr-only
    /// operation. Calling `init_logging` itself here would install a
    /// process-global stderr subscriber that spams every later test in this
    /// binary, so only the branch point is exercised.
    #[test]
    fn an_uncreatable_log_directory_fails_the_writer_not_the_process() {
        let dir = tempfile::tempdir().unwrap();
        // A *file* where the data dir should be makes `logs/` uncreatable.
        let occupied = dir.path().join("data");
        fs::write(&occupied, b"not a directory").unwrap();
        assert!(open_log_writer(&occupied).is_err());
    }

    /// Smoke: a `tracing::warn!` dispatched through the real layer stack lands
    /// in the file. Uses a scoped dispatcher rather than `init_logging` so the
    /// test binary's process-global subscriber slot stays free.
    #[test]
    fn a_warn_event_lands_in_the_log_file() {
        let dir = tempfile::tempdir().unwrap();
        let writer = open_log_writer(dir.path()).unwrap();
        let subscriber = tracing_subscriber::registry()
            .with(EnvFilter::new(DEFAULT_DIRECTIVES))
            .with(
                tracing_subscriber::fmt::layer()
                    .compact()
                    .with_ansi(false)
                    .with_writer(writer),
            );
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!("gateway endpoint unreachable");
        });
        let contents = fs::read_to_string(dir.path().join("logs/tidebreak.log")).unwrap();
        assert!(contents.contains("WARN"), "level missing: {contents:?}");
        assert!(
            contents.contains("gateway endpoint unreachable"),
            "message missing: {contents:?}"
        );
    }
}
