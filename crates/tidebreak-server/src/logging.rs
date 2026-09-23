//! Process-wide tracing subscriber for the desktop shell and `tidebreak serve`.
//!
//! Events land in two bounded files under the profile data directory:
//! `logs/tidebreak.log` for people and `logs/tidebreak.events.jsonl` for tools.
//! The human log keeps one rotated file and the structured file keeps
//! [`EVENT_LOG_ROTATIONS`]. The structured file includes span-close records
//! and timing events under the `tidebreak_diagnostics` target; those
//! high-volume events stay out of the human log and debug stderr mirror.
//!
//! A background thread writes each file through a buffer, so a tracing call
//! never waits on the disk from a runtime worker. [`shutdown`] writes out
//! whatever is still buffered; call it on the way out of the process.
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
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_appender::non_blocking::{NonBlocking, NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::filter::{filter_fn, FilterExt as _, Targets};
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

type BoxedRegistryLayer =
    Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync>;

/// Rotate `tidebreak.log` to `tidebreak.log.1` once it would pass this size.
pub const LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;

/// How many rotated human log files are kept beside the live one.
pub const LOG_ROTATIONS: usize = 1;

/// Rotate the structured event file at a larger cap because JSON timing events
/// and span context cost more bytes than compact text events.
pub const EVENT_LOG_ROTATE_BYTES: u64 = 10 * 1024 * 1024;

/// How many rotated structured event files are kept beside the live one, so
/// a diagnostic bundle reaches back further than the latest busy stretch.
pub const EVENT_LOG_ROTATIONS: usize = 4;

/// How much a log file buffers before its writer thread writes it out. The
/// thread also writes what it has each time it runs out of queued lines, so
/// the buffer only coalesces bursts.
const LOG_BUFFER_BYTES: usize = 64 * 1024;

/// Environment variable holding `EnvFilter` directives that override
/// [`DEFAULT_DIRECTIVES`].
const LOG_ENV_VAR: &str = "TIDEBREAK_LOG";

/// Optional `EnvFilter` directives for the structured event file.
const DIAGNOSTIC_LOG_ENV_VAR: &str = "TIDEBREAK_DIAGNOSTICS_LOG";

/// `info` for the workspace's own crates, `warn` for dependencies.
///
/// sqlx warns once per waiter whenever the connection pool is slow, which can
/// fill the log during one stall. Those warnings stay out of the human log;
/// [`PoolWaitCounter`] folds them into one line a minute instead.
const DEFAULT_DIRECTIVES: &str = "warn,sqlx::pool::acquire=error,tidebreak_cli=info,\
    tidebreak_code_delivery=info,\
    tidebreak_code_execution=info,tidebreak_code_remote=info,tidebreak_core=info,\
    tidebreak_desktop=info,tidebreak_egress=info,\
    tidebreak_gateway_runtime=info,tidebreak_harness=info,tidebreak_host_broker=info,\
    tidebreak_managed_node=info,\
    tidebreak_mcp=info,\
    tidebreak_router=info,tidebreak_sandbox_agent=info,tidebreak_sandbox_protocol=info,\
    tidebreak_sandbox_runtime=info,tidebreak_server=info,tidebreak_server_core=info,\
    tidebreak_shell_policy=info,\
    tidebreak_supervised_agent=info,tidebreak_whisper=info,tidebreak_worker_runtime=info";

/// Keep the structured file focused on purpose-built, payload-free events.
pub(crate) const DEFAULT_DIAGNOSTIC_DIRECTIVES: &str = "off,tidebreak_diagnostics=info";

/// The target sqlx logs a slow connection-pool acquisition under.
const POOL_ACQUIRE_TARGET: &str = "sqlx::pool::acquire";

/// How often the pool-wait summary is written while waits keep happening.
const POOL_WAIT_SUMMARY_INTERVAL: Duration = Duration::from_secs(60);

/// The writer threads behind the installed log files.
///
/// Held for the life of the process, so the threads keep running. Dropping a
/// guard writes out that file's queued lines and stops its thread, which is
/// what [`shutdown`] does.
static LOG_WRITERS: Mutex<Vec<WorkerGuard>> = Mutex::new(Vec::new());

/// Install the process-global subscriber, writing the human and structured
/// files under `data_dir/logs`.
///
/// Infallible by design: if the log directory or file cannot be created the
/// subscriber degrades to stderr-only, and if a subscriber is already
/// installed (tests, a second call) the call is a no-op. Logging must never
/// block boot.
pub fn init_logging(data_dir: &Path) {
    let mut layers: Vec<BoxedRegistryLayer> = Vec::new();
    let mut writers = Vec::new();
    let human = human_filter();
    let _text_available = match open_log_writer(data_dir) {
        Ok(writer) => {
            layers.push(
                tracing_subscriber::fmt::layer()
                    .compact()
                    .with_ansi(false)
                    .with_writer(in_background(writer, "tidebreak-log", &mut writers))
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
        Ok(writer) => layers.push(structured_layer(
            in_background(writer, "tidebreak-event-log", &mut writers),
            diagnostic_filter(),
        )),
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
    install(layers, writers);
}

/// Install the file-only subscriber used by commands whose stdout is data.
///
/// Unlike [`init_logging`] there is no stderr mirror. If neither file opens,
/// the function installs an empty subscriber rather than corrupting command
/// output.
pub fn init_logging_file_only(data_dir: &Path) {
    let mut layers: Vec<BoxedRegistryLayer> = Vec::new();
    let mut writers = Vec::new();
    if let Ok(writer) = open_log_writer(data_dir) {
        layers.push(
            tracing_subscriber::fmt::layer()
                .compact()
                .with_ansi(false)
                .with_writer(in_background(writer, "tidebreak-log", &mut writers))
                .with_filter(human_filter())
                .boxed(),
        );
    }
    if let Ok(writer) = open_event_writer(data_dir) {
        layers.push(structured_layer(
            in_background(writer, "tidebreak-event-log", &mut writers),
            diagnostic_filter(),
        ));
    }
    install(layers, writers);
}

/// Write out every queued log line and stop the log writer threads.
///
/// Call once, on the way out of the process: each thread gets about a second
/// to drain, and lines logged after this call are dropped. Calling it again,
/// or before logging was installed, does nothing.
pub fn shutdown() {
    let writers = std::mem::take(&mut *LOG_WRITERS.lock().unwrap_or_else(PoisonError::into_inner));
    drop(writers);
}

/// Install `layers` as the global subscriber, plus the pool-wait counter that
/// every entry point shares.
fn install(mut layers: Vec<BoxedRegistryLayer>, writers: Vec<WorkerGuard>) {
    let waits = Arc::new(PoolWaits::default());
    layers.push(
        PoolWaitCounter(waits.clone())
            .with_filter(pool_wait_filter())
            .boxed(),
    );
    if tracing_subscriber::registry()
        .with(layers)
        .try_init()
        .is_ok()
    {
        LOG_WRITERS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(writers);
        spawn_pool_wait_summary(waits);
    }
    // Otherwise another subscriber already owns the process, and dropping
    // `writers` stops the threads nothing will ever write to.
}

/// Hand a file writer to its own thread and keep that thread's guard.
///
/// The thread drops a line rather than block the caller if it falls more
/// than its queue behind; logging must never stall the work it describes.
fn in_background(
    writer: RotatingFileWriter,
    thread_name: &str,
    writers: &mut Vec<WorkerGuard>,
) -> NonBlocking {
    let (writer, guard) = NonBlockingBuilder::default()
        .thread_name(thread_name)
        .finish(writer);
    writers.push(guard);
    writer
}

/// The structured JSON layer, shared by both entry points and the tests.
pub(crate) fn structured_layer<W>(writer: W, filter: EnvFilter) -> BoxedRegistryLayer
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
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
        .with_filter(filter)
        .boxed()
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

/// Only the slow-acquire warnings sqlx logs reach the pool-wait counter.
fn pool_wait_filter() -> Targets {
    Targets::new().with_target(POOL_ACQUIRE_TARGET, Level::WARN)
}

/// Slow connection-pool waits counted since the last summary line.
#[derive(Default)]
struct PoolWaits {
    count: AtomicU64,
    longest_micros: AtomicU64,
    threshold_micros: AtomicU64,
}

/// One summary of slow connection-pool waits.
#[derive(Debug, PartialEq)]
struct PoolWaitSummary {
    count: u64,
    longest: Duration,
    threshold: Duration,
}

impl PoolWaits {
    fn record(&self, waited: Duration, threshold: Duration) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.longest_micros
            .fetch_max(duration_micros(waited), Ordering::Relaxed);
        self.threshold_micros
            .fetch_max(duration_micros(threshold), Ordering::Relaxed);
    }

    /// The waits counted since the last call, if there were any.
    fn take(&self) -> Option<PoolWaitSummary> {
        let count = self.count.swap(0, Ordering::Relaxed);
        let longest = self.longest_micros.swap(0, Ordering::Relaxed);
        let threshold = self.threshold_micros.swap(0, Ordering::Relaxed);
        (count > 0).then(|| PoolWaitSummary {
            count,
            longest: Duration::from_micros(longest),
            threshold: Duration::from_micros(threshold),
        })
    }
}

fn duration_micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

/// Counts the slow-acquire warnings sqlx logs, one per waiter, so the human
/// log can carry one summary a minute instead of thousands of lines.
struct PoolWaitCounter(Arc<PoolWaits>);

impl<S: Subscriber> Layer<S> for PoolWaitCounter {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let mut wait = SlowAcquireFields::default();
        event.record(&mut wait);
        if let Some(waited) = wait.acquired_after {
            self.0.record(waited, wait.threshold.unwrap_or_default());
        }
    }
}

/// The two numbers sqlx attaches to a slow-acquire warning.
#[derive(Default)]
struct SlowAcquireFields {
    acquired_after: Option<Duration>,
    threshold: Option<Duration>,
}

impl Visit for SlowAcquireFields {
    fn record_f64(&mut self, field: &Field, value: f64) {
        let Ok(seconds) = Duration::try_from_secs_f64(value) else {
            return;
        };
        match field.name() {
            "acquired_after_secs" => self.acquired_after = Some(seconds),
            "slow_acquire_threshold_secs" => self.threshold = Some(seconds),
            _ => {}
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}

/// Write one pool-wait summary a minute, and nothing in a quiet minute.
fn spawn_pool_wait_summary(waits: Arc<PoolWaits>) {
    let spawned = std::thread::Builder::new()
        .name("tidebreak-pool-waits".to_owned())
        .spawn(move || loop {
            std::thread::sleep(POOL_WAIT_SUMMARY_INTERVAL);
            if let Some(summary) = waits.take() {
                tracing::warn!(
                    waits = summary.count,
                    longest_wait_secs = summary.longest.as_secs_f64(),
                    "{} database calls waited more than {:.0}s for a pooled connection in the last \
                     minute; the longest waited {:.1}s",
                    summary.count,
                    summary.threshold.as_secs_f64(),
                    summary.longest.as_secs_f64(),
                );
            }
        });
    if let Err(error) = spawned {
        eprintln!("tidebreak: database pool-wait summary unavailable ({error})");
    }
}

/// Create `logs/` under the data directory and open the bounded writer.
fn open_log_writer(data_dir: &Path) -> io::Result<RotatingFileWriter> {
    let logs = data_dir.join("logs");
    fs::create_dir_all(&logs)?;
    RotatingFileWriter::new(logs.join("tidebreak.log"), LOG_ROTATE_BYTES, LOG_ROTATIONS)
}

pub(crate) fn open_event_writer(data_dir: &Path) -> io::Result<RotatingFileWriter> {
    let logs = data_dir.join("logs");
    fs::create_dir_all(&logs)?;
    RotatingFileWriter::new(
        logs.join("tidebreak.events.jsonl"),
        EVENT_LOG_ROTATE_BYTES,
        EVENT_LOG_ROTATIONS,
    )
}

/// A size-capped log file with a fixed number of rotation slots.
///
/// Appends to `path`; once a write would push the file past `cap` bytes, each
/// rotated file moves up one slot (`<path>.1` to `<path>.2`, and so on), the
/// oldest is deleted, the live file becomes `<path>.1`, and a fresh file
/// starts. Best-effort throughout: a failed rotation or reopen silently drops
/// output rather than surfacing errors into the subscriber.
#[derive(Clone)]
pub(crate) struct RotatingFileWriter {
    inner: Arc<Mutex<RotatingFile>>,
}

struct RotatingFile {
    path: PathBuf,
    cap: u64,
    /// How many rotated files to keep.
    rotations: usize,
    /// `None` after a reopen failure; writes are then dropped.
    file: Option<BufWriter<File>>,
    written: u64,
}

impl RotatingFileWriter {
    fn new(path: PathBuf, cap: u64, rotations: usize) -> io::Result<Self> {
        let file = open_private_log_file(&path, true)?;
        let written = file.metadata()?.len();
        Ok(Self {
            inner: Arc::new(Mutex::new(RotatingFile {
                path,
                cap,
                rotations: rotations.max(1),
                file: Some(BufWriter::with_capacity(LOG_BUFFER_BYTES, file)),
                written,
            })),
        })
    }
}

impl RotatingFile {
    /// Shift the rotated files up one slot, move the live file into the first
    /// slot, and start a fresh one.
    ///
    /// The handle is dropped before the renames so Windows can move the file.
    fn rotate(&mut self) {
        if let Some(mut file) = self.file.take() {
            let _ = file.flush();
            drop(file);
        }
        let _ = fs::remove_file(self.slot(self.rotations));
        for slot in (1..self.rotations).rev() {
            let _ = fs::rename(self.slot(slot), self.slot(slot + 1));
        }
        let _ = fs::rename(&self.path, self.slot(1));
        self.file = open_private_log_file(&self.path, false)
            .ok()
            .map(|file| BufWriter::with_capacity(LOG_BUFFER_BYTES, file));
        self.written = 0;
    }

    fn slot(&self, slot: usize) -> PathBuf {
        let mut rotated = self.path.clone().into_os_string();
        rotated.push(format!(".{slot}"));
        PathBuf::from(rotated)
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
        let mut writer = RotatingFileWriter::new(path.clone(), 64, 1).unwrap();

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

    /// With one slot, a second rotation replaces the previous `.1` file
    /// instead of growing a chain, keeping the on-disk footprint bounded.
    #[test]
    fn a_later_rotation_replaces_the_previous_slot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tidebreak.log");
        let mut writer = RotatingFileWriter::new(path.clone(), 8, 1).unwrap();

        writer.write_all(b"first").unwrap();
        writer.write_all(b"second").unwrap(); // rotates: .1 = "first"
        writer.write_all(b"third").unwrap(); // rotates: .1 = "second"
        writer.flush().unwrap();

        let rotated = fs::read(dir.path().join("tidebreak.log.1")).unwrap();
        assert_eq!(rotated, b"second");
        assert_eq!(fs::read(&path).unwrap(), b"third");
        assert!(!dir.path().join("tidebreak.log.2").exists());
    }

    /// Several slots hold the newest rotations, oldest last, and the file
    /// that would fall past the last slot is deleted.
    #[test]
    fn rotations_shift_through_every_slot_and_drop_the_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tidebreak.events.jsonl");
        let mut writer = RotatingFileWriter::new(path.clone(), 4, 3).unwrap();

        for line in [b"one!", b"two!", b"thr!", b"fou!", b"fiv!"] {
            writer.write_all(line).unwrap();
        }
        writer.flush().unwrap();

        let slot =
            |slot: usize| fs::read(dir.path().join(format!("tidebreak.events.jsonl.{slot}")));
        assert_eq!(fs::read(&path).unwrap(), b"fiv!");
        assert_eq!(slot(1).unwrap(), b"fou!");
        assert_eq!(slot(2).unwrap(), b"thr!");
        assert_eq!(slot(3).unwrap(), b"two!");
        assert!(slot(4).is_err(), "the oldest rotation must be deleted");
    }

    #[cfg(unix)]
    #[test]
    fn log_files_are_owner_only_before_and_after_rotation() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tidebreak.log");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let mut writer = RotatingFileWriter::new(path.clone(), 8, 1).unwrap();

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

        let result = RotatingFileWriter::new(path, 64, 1);

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
    /// in the file once the background writer drains. Uses a scoped dispatcher
    /// rather than `init_logging` so the test binary's process-global
    /// subscriber slot stays free.
    #[test]
    fn a_warn_event_lands_in_the_log_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut writers = Vec::new();
        let writer = in_background(
            open_log_writer(dir.path()).unwrap(),
            "tidebreak-log-test",
            &mut writers,
        );
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
        // Dropping the guard is the shutdown path: it drains the queue.
        drop(writers);
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
            let manifest = std::fs::read_to_string(entry.path().join("Cargo.toml"))
                .expect("read crate manifest");
            let name = manifest
                .lines()
                .skip_while(|line| line.trim() != "[package]")
                .skip(1)
                .take_while(|line| !line.trim_start().starts_with('['))
                .find_map(|line| {
                    line.trim()
                        .strip_prefix("name = \"")
                        .and_then(|name| name.strip_suffix('"'))
                })
                .expect("workspace crate package name")
                .replace('-', "_");
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
        let subscriber = tracing_subscriber::registry().with(structured_layer(
            writer.clone(),
            EnvFilter::new(DEFAULT_DIAGNOSTIC_DIRECTIVES),
        ));
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
        writer.clone().flush().unwrap();
        let contents = fs::read_to_string(dir.path().join("logs/tidebreak.events.jsonl")).unwrap();
        assert!(contents.contains("http.server.request.completed"));
        assert!(contents.contains("close"), "span close missing: {contents}");
        assert!(!contents.contains("ordinary warning"));
    }

    /// sqlx warns once per waiter when the pool is slow. The human log drops
    /// those lines and the counter turns them into one summary, with the
    /// count and the longest wait.
    #[test]
    fn slow_pool_waits_become_one_summary_instead_of_a_line_each() {
        let human_targets = Arc::new(Mutex::new(Vec::new()));
        let waits = Arc::new(PoolWaits::default());
        let subscriber = tracing_subscriber::registry()
            .with(
                TargetProbe(human_targets.clone()).with_filter(EnvFilter::new(DEFAULT_DIRECTIVES)),
            )
            .with(PoolWaitCounter(waits.clone()).with_filter(pool_wait_filter()));
        tracing::subscriber::with_default(subscriber, || {
            for waited in [2.5, 61.0, 3.0] {
                tracing::warn!(
                    target: "sqlx::pool::acquire",
                    acquired_after_secs = waited,
                    slow_acquire_threshold_secs = 2.0,
                    "acquired connection, but time to acquire exceeded slow threshold"
                );
            }
            tracing::warn!(target: "sqlx::query", "an unrelated sqlx warning");
        });

        assert_eq!(
            *human_targets.lock().unwrap(),
            ["sqlx::query"],
            "per-waiter pool warnings must stay out of the human log"
        );
        assert_eq!(
            waits.take(),
            Some(PoolWaitSummary {
                count: 3,
                longest: Duration::from_secs(61),
                threshold: Duration::from_secs(2),
            })
        );
        assert_eq!(waits.take(), None, "a quiet minute writes no summary");
    }

    /// Records the target of every event its filter lets through.
    struct TargetProbe(Arc<Mutex<Vec<String>>>);

    impl<S: Subscriber> Layer<S> for TargetProbe {
        fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
            self.0
                .lock()
                .unwrap()
                .push(event.metadata().target().to_owned());
        }
    }
}
