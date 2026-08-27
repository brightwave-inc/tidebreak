//! Process-wide tracing subscriber for the desktop shell and `tidebreak serve`.
//!
//! Events land in two bounded files under the profile data directory:
//! `logs/tidebreak.log` for people and `logs/tidebreak.events.jsonl` for tools.
//! Each file has one rotation slot. The structured file includes span-close
//! records and timing events under the `tidebreak_diagnostics` target; those
//! high-volume events stay out of the human log and debug stderr mirror.
//!
//! The default level policy is `info` for the workspace's own `tidebreak*`
//! crates and `warn` for everything else. The `TIDEBREAK_LOG` environment
//! variable overrides it with standard `tracing_subscriber::EnvFilter`
//! directives (e.g. `TIDEBREAK_LOG=debug` or
//! `TIDEBREAK_LOG=warn,tidebreak_server=trace`); an invalid spec falls back to
//! the default rather than failing boot. `TIDEBREAK_DIAGNOSTICS_LOG` controls
//! the structured file separately.
//!
//! # Redaction posture
//!
//! The workspace's emit sites are disciplined: reqwest errors are URL-stripped
//! before logging, policy diagnostics name environment *variables* rather than
//! values, and no secret or token ever appears in an error's `Display` text.
//! The human layer uses the compact formatter and omits diagnostic timing
//! spans. The structured layer records event fields, span context, and span
//! close records, so diagnostic instrumentation must use names, counts, and
//! durations rather than prompts, tool payloads, URL queries, credentials, or
//! file contents. Every ordinary log site must keep the same boundary.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::filter::{filter_fn, FilterExt as _};
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer as _;

type BoxedRegistryLayer =
    Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync>;

/// Rotate `tidebreak.log` to `tidebreak.log.1` once it would pass this size.
pub const LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;

/// Rotate the structured event file at a larger cap because JSON timing events
/// and span context cost more bytes than compact text events.
pub const EVENT_LOG_ROTATE_BYTES: u64 = 10 * 1024 * 1024;

/// Environment variable holding `EnvFilter` directives that override
/// [`DEFAULT_DIRECTIVES`].
const LOG_ENV_VAR: &str = "TIDEBREAK_LOG";

/// Optional `EnvFilter` directives for the structured event file.
const DIAGNOSTIC_LOG_ENV_VAR: &str = "TIDEBREAK_DIAGNOSTICS_LOG";

/// `info` for the workspace's own crates, `warn` for dependencies.
const DEFAULT_DIRECTIVES: &str = "warn,tidebreak_cli=info,tidebreak_code_execution=info,\
    tidebreak_core=info,tidebreak_desktop=info,tidebreak_egress=info,\
    tidebreak_harness=info,tidebreak_host_broker=info,tidebreak_managed_node=info,\
    tidebreak_mcp=info,\
    tidebreak_router=info,tidebreak_sandbox_agent=info,tidebreak_sandbox_protocol=info,\
    tidebreak_server=info,tidebreak_shell_policy=info,\
    tidebreak_supervised_agent=info,tidebreak_whisper=info";

/// Keep the structured file focused on purpose-built, payload-free events.
const DEFAULT_DIAGNOSTIC_DIRECTIVES: &str = "off,tidebreak_diagnostics=info";

/// Install the process-global subscriber, writing the human and structured
/// files under `data_dir/logs`.
///
/// Infallible by design: if the log directory or file cannot be created the
/// subscriber degrades to stderr-only, and if a subscriber is already
/// installed (tests, a second call) the call is a no-op. Logging must never
/// block boot.
pub fn init_logging(data_dir: &Path) {
    let mut layers: Vec<BoxedRegistryLayer> = Vec::new();
    let human = human_filter();
    let _text_available = match open_log_writer(data_dir) {
        Ok(writer) => {
            layers.push(
                tracing_subscriber::fmt::layer()
                    .compact()
                    .with_ansi(false)
                    .with_writer(writer)
                    .with_filter(human.clone())
                    .boxed(),
            );
            true
        }
        Err(error) => {
            eprintln!(
                "tidebreak: profile log file unavailable ({error}); human logging falls back to stderr"
            );
            false
        }
    };
    match open_event_writer(data_dir) {
        Ok(writer) => layers.push(
            tracing_subscriber::fmt::layer()
                .json()
                .flatten_event(true)
                .with_current_span(true)
                .with_span_list(true)
                .with_span_events(FmtSpan::CLOSE)
                .with_thread_ids(true)
                .with_thread_names(true)
                .with_file(true)
                .with_line_number(true)
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(diagnostic_filter())
                .boxed(),
        ),
        Err(error) => {
            eprintln!("tidebreak: structured diagnostic log unavailable ({error})");
        }
    }
    #[cfg(debug_assertions)]
    layers.push(
        tracing_subscriber::fmt::layer()
            .compact()
            .with_ansi(false)
            .with_writer(io::stderr)
            .with_filter(human)
            .boxed(),
    );
    #[cfg(not(debug_assertions))]
    if !_text_available {
        layers.push(
            tracing_subscriber::fmt::layer()
                .compact()
                .with_ansi(false)
                .with_writer(io::stderr)
                .with_filter(human)
                .boxed(),
        );
    }
    let subscriber = tracing_subscriber::registry().with(layers);
    let _ = subscriber.try_init();
}

/// Install the file-only subscriber used by commands whose stdout is data.
///
/// Unlike [`init_logging`] there is no stderr mirror. If neither file opens,
/// the function installs an empty subscriber rather than corrupting command
/// output.
pub fn init_logging_file_only(data_dir: &Path) {
    let mut layers: Vec<BoxedRegistryLayer> = Vec::new();
    if let Ok(writer) = open_log_writer(data_dir) {
        layers.push(
            tracing_subscriber::fmt::layer()
                .compact()
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(human_filter())
                .boxed(),
        );
    }
    if let Ok(writer) = open_event_writer(data_dir) {
        layers.push(
            tracing_subscriber::fmt::layer()
                .json()
                .flatten_event(true)
                .with_current_span(true)
                .with_span_list(true)
                .with_span_events(FmtSpan::CLOSE)
                .with_thread_ids(true)
                .with_thread_names(true)
                .with_file(true)
                .with_line_number(true)
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(diagnostic_filter())
                .boxed(),
        );
    }
    let subscriber = tracing_subscriber::registry().with(layers);
    let _ = subscriber.try_init();
}

/// The active filter: `TIDEBREAK_LOG` when set and valid, the default policy
/// otherwise.
fn env_filter(variable: &str, default: &str) -> EnvFilter {
    match std::env::var(variable) {
        Ok(spec) if !spec.trim().is_empty() => EnvFilter::try_new(&spec).unwrap_or_else(|error| {
            eprintln!("tidebreak: invalid {variable} ({error}); using the default filter");
            EnvFilter::new(default)
        }),
        _ => EnvFilter::new(default),
    }
}

fn human_filter() -> impl tracing_subscriber::layer::Filter<tracing_subscriber::Registry> + Clone {
    env_filter(LOG_ENV_VAR, DEFAULT_DIRECTIVES).and(filter_fn(|metadata| {
        metadata.target() != crate::diagnostics::EVENT_TARGET
    }))
}

fn diagnostic_filter() -> EnvFilter {
    env_filter(DIAGNOSTIC_LOG_ENV_VAR, DEFAULT_DIAGNOSTIC_DIRECTIVES)
}

/// Create `logs/` under the data directory and open the bounded writer.
fn open_log_writer(data_dir: &Path) -> io::Result<RotatingFileWriter> {
    let logs = data_dir.join("logs");
    fs::create_dir_all(&logs)?;
    RotatingFileWriter::new(logs.join("tidebreak.log"), LOG_ROTATE_BYTES)
}

fn open_event_writer(data_dir: &Path) -> io::Result<RotatingFileWriter> {
    let logs = data_dir.join("logs");
    fs::create_dir_all(&logs)?;
    RotatingFileWriter::new(logs.join("tidebreak.events.jsonl"), EVENT_LOG_ROTATE_BYTES)
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
        let file = open_private_log_file(&path, true)?;
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
        self.file = open_private_log_file(&self.path, false).ok();
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

fn open_private_log_file(path: &Path, append: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).write(true);
    if append {
        options.append(true);
    } else {
        options.truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
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

    #[cfg(unix)]
    #[test]
    fn log_files_are_owner_only_before_and_after_rotation() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tidebreak.log");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let mut writer = RotatingFileWriter::new(path.clone(), 8).unwrap();

        writer.write_all(b"first").unwrap();
        writer.write_all(b"second").unwrap();
        writer.flush().unwrap();

        for path in [path, dir.path().join("tidebreak.log.1")] {
            let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[cfg(unix)]
    #[test]
    fn log_writer_refuses_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination");
        let path = dir.path().join("tidebreak.log");
        fs::write(&destination, b"private").unwrap();
        std::os::unix::fs::symlink(&destination, &path).unwrap();

        let result = RotatingFileWriter::new(path, 64);

        assert!(result.is_err());
        assert_eq!(fs::read(destination).unwrap(), b"private");
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
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .compact()
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(EnvFilter::new(DEFAULT_DIRECTIVES)),
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

    /// Every workspace crate needs a directive, or it sits at the global
    /// `warn` floor and anything it logs below that is invisible. That is how
    /// `tidebreak_harness` came to swallow unrecognized engine events: the
    /// crate was absent, so its diagnostics never reached the log.
    #[test]
    fn every_workspace_crate_has_a_log_directive() {
        let listed: std::collections::HashSet<&str> = DEFAULT_DIRECTIVES
            .split(',')
            .filter_map(|directive| directive.split('=').next())
            .map(str::trim)
            .collect();

        let crates_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/ is the manifest's parent");
        let mut missing = Vec::new();
        for entry in std::fs::read_dir(crates_dir).expect("read crates/") {
            let entry = entry.expect("crate dir entry");
            if !entry.path().join("Cargo.toml").is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().replace('-', "_");
            if !listed.contains(name.as_str()) {
                missing.push(name);
            }
        }
        missing.sort();
        assert!(
            missing.is_empty(),
            "workspace crates with no log directive, so they sit at warn: {missing:?}"
        );
    }

    #[test]
    fn structured_log_keeps_timing_events_and_span_close_records() {
        let dir = tempfile::tempdir().unwrap();
        let writer = open_event_writer(dir.path()).unwrap();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .json()
                .flatten_event(true)
                .with_current_span(true)
                .with_span_list(true)
                .with_span_events(FmtSpan::CLOSE)
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(EnvFilter::new(DEFAULT_DIAGNOSTIC_DIRECTIVES)),
        );
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!("ordinary warning");
            let span = tracing::info_span!(
                target: crate::diagnostics::EVENT_TARGET,
                "http.server.request",
                http.route = "/healthz"
            );
            span.in_scope(|| {
                tracing::info!(
                    target: crate::diagnostics::EVENT_TARGET,
                    event_name = "http.server.request.completed",
                    "request completed"
                );
            });
        });
        let contents = fs::read_to_string(dir.path().join("logs/tidebreak.events.jsonl")).unwrap();
        assert!(contents.contains("http.server.request.completed"));
        assert!(contents.contains("close"), "span close missing: {contents}");
        assert!(!contents.contains("ordinary warning"));
    }
}
