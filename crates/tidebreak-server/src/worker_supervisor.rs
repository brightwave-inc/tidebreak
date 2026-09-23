//! Keep the server's long-lived background workers running.
//!
//! The server starts about fifteen workers at boot: the chat turn worker, the
//! sandbox workers, blob retirement, the approval judge, the memory sweep, the
//! MCP supervisor, and more. Each is meant to run until the process stops.
//! Before this module each was a bare `tokio::spawn`, so a panic or an
//! unexpected return ended it silently, and the server kept answering requests
//! while, say, no chat turn was ever claimed again.
//!
//! [`spawn_supervised`] runs a worker, logs how it stopped, and starts it again
//! after a capped, jittered wait. Shutdown stops the loop: once
//! [`WorkerHealth::begin_shutdown`] runs, a worker that stops stays stopped.
//! [`WorkerHealth`] records what each worker is doing, and the diagnostics
//! snapshot reports it.

use std::collections::BTreeMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use futures::FutureExt as _;
use serde::Serialize;

use crate::retry::LaneBackoff;

/// How a supervised worker is restarted after it stops.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RestartPolicy {
    /// The wait before the first restart of a failure episode.
    pub(crate) initial: Duration,
    /// The longest wait between two restarts.
    pub(crate) cap: Duration,
    /// A worker that ran at least this long before it stopped starts a new
    /// failure episode, so one crash a day never waits out the cap.
    pub(crate) healthy_run: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(1),
            cap: Duration::from_secs(60),
            healthy_run: Duration::from_secs(5 * 60),
        }
    }
}

/// The most characters of a worker's last error the snapshot keeps.
const MAX_ERROR_CHARS: usize = 512;

/// What a supervised worker is doing right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    /// The worker is running.
    Running,
    /// The worker stopped and is waiting to start again.
    Restarting,
    /// The worker stopped for good, because the server is shutting down.
    Stopped,
}

/// One worker's health, as the diagnostics snapshot reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorkerHealthSnapshot {
    pub name: String,
    pub state: WorkerState,
    /// How many times the worker stopped and was started again.
    pub restarts: u64,
    /// How the worker last stopped: a panic message, or a note that it
    /// returned. `None` while it has never stopped.
    pub last_error: Option<String>,
    /// When the worker last stopped.
    pub last_stopped_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
struct WorkerRecord {
    state: WorkerState,
    restarts: u64,
    last_error: Option<String>,
    last_stopped_at: Option<DateTime<Utc>>,
}

/// The health of every supervised worker in one server.
#[derive(Default)]
pub struct WorkerHealth {
    shutting_down: AtomicBool,
    workers: Mutex<BTreeMap<&'static str, WorkerRecord>>,
}

impl WorkerHealth {
    /// Stop restarting workers. Call before aborting them, so a worker that
    /// stops during shutdown is recorded as stopped rather than restarted.
    pub fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
    }

    fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    /// Record every worker as stopped. Shutdown aborts the supervisors, and an
    /// aborted task runs none of its own bookkeeping.
    pub fn mark_all_stopped(&self) {
        for record in self.lock().values_mut() {
            record.state = WorkerState::Stopped;
        }
    }

    /// Every worker's health, sorted by name.
    pub fn snapshot(&self) -> Vec<WorkerHealthSnapshot> {
        self.lock()
            .iter()
            .map(|(name, record)| WorkerHealthSnapshot {
                name: (*name).to_owned(),
                state: record.state,
                restarts: record.restarts,
                last_error: record.last_error.clone(),
                last_stopped_at: record.last_stopped_at,
            })
            .collect()
    }

    fn running(&self, name: &'static str) {
        self.lock()
            .entry(name)
            .and_modify(|record| record.state = WorkerState::Running)
            .or_insert(WorkerRecord {
                state: WorkerState::Running,
                restarts: 0,
                last_error: None,
                last_stopped_at: None,
            });
    }

    fn stopped(&self, name: &'static str, error: &str, state: WorkerState) {
        let mut workers = self.lock();
        let record = workers.entry(name).or_insert(WorkerRecord {
            state,
            restarts: 0,
            last_error: None,
            last_stopped_at: None,
        });
        record.state = state;
        if state == WorkerState::Restarting {
            record.restarts = record.restarts.saturating_add(1);
        }
        record.last_error = Some(bounded(error));
        record.last_stopped_at = Some(Utc::now());
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<&'static str, WorkerRecord>> {
        self.workers.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Run a worker on its own task and keep it running.
///
/// `start` builds a fresh run of the worker each time: usually a clone of the
/// worker's handles and a call to its `run`. When a run panics or returns, the
/// supervisor logs it, records it in `health`, waits, and starts another,
/// unless the server is shutting down. Aborting the returned task stops the
/// worker with it.
pub(crate) fn spawn_supervised<F, Fut>(
    health: Arc<WorkerHealth>,
    name: &'static str,
    start: F,
) -> tokio::task::JoinHandle<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(supervise(health, name, RestartPolicy::default(), start))
}

pub(crate) async fn supervise<F, Fut>(
    health: Arc<WorkerHealth>,
    name: &'static str,
    policy: RestartPolicy,
    mut start: F,
) where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    let mut backoff = LaneBackoff::new(policy.initial, policy.cap);
    loop {
        health.running(name);
        let started = Instant::now();
        // A panic has already been written to the log by the process panic
        // hook, with its location and backtrace. This only names the worker.
        let stop = match AssertUnwindSafe(start()).catch_unwind().await {
            Ok(()) => "the worker returned".to_owned(),
            Err(payload) => format!(
                "the worker panicked: {}",
                crate::logging::panic_payload_message(payload.as_ref())
            ),
        };
        if health.is_shutting_down() {
            health.stopped(name, &stop, WorkerState::Stopped);
            return;
        }
        if started.elapsed() >= policy.healthy_run {
            backoff.reset();
        }
        let delay = backoff.next_delay();
        tracing::error!(
            worker = name,
            restart_in_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            "background worker {name} stopped ({stop}); starting it again"
        );
        health.stopped(name, &stop, WorkerState::Restarting);
        tokio::time::sleep(delay).await;
        if health.is_shutting_down() {
            health.stopped(name, &stop, WorkerState::Stopped);
            return;
        }
    }
}

/// A worker's stop reason as the snapshot keeps it: scrubbed like any text
/// from outside the log's own emit sites, cut short, and on one line.
fn bounded(error: &str) -> String {
    crate::logging::scrub_log_text(error, MAX_ERROR_CHARS)
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    fn policy() -> RestartPolicy {
        RestartPolicy {
            initial: Duration::from_secs(1),
            cap: Duration::from_secs(8),
            healthy_run: Duration::from_secs(600),
        }
    }

    fn worker(health: &WorkerHealth, name: &str) -> WorkerHealthSnapshot {
        health
            .snapshot()
            .into_iter()
            .find(|worker| worker.name == name)
            .expect("the worker is recorded")
    }

    /// The bug this module fixes: a worker that panicked stayed dead. Here it
    /// panics twice and runs on its third start, and the snapshot shows both
    /// restarts and the panic message.
    #[tokio::test(start_paused = true)]
    async fn a_panicking_worker_is_restarted_and_its_health_says_so() {
        let health = Arc::new(WorkerHealth::default());
        let starts = Arc::new(AtomicUsize::new(0));
        let counted = starts.clone();
        let supervisor = tokio::spawn(supervise(
            health.clone(),
            "turn_worker",
            policy(),
            move || {
                let start = counted.fetch_add(1, Ordering::SeqCst);
                async move {
                    if start < 2 {
                        panic!("injected failure {start}");
                    }
                    std::future::pending::<()>().await;
                }
            },
        ));

        tokio::time::sleep(Duration::from_secs(30)).await;

        assert_eq!(starts.load(Ordering::SeqCst), 3);
        let turn_worker = worker(&health, "turn_worker");
        assert_eq!(turn_worker.state, WorkerState::Running);
        assert_eq!(turn_worker.restarts, 2);
        assert_eq!(
            turn_worker.last_error.as_deref(),
            Some("the worker panicked: injected failure 1")
        );
        assert!(turn_worker.last_stopped_at.is_some());
        supervisor.abort();
    }

    /// A worker that keeps stopping waits longer each time, never longer than
    /// the cap, and a worker that returns counts as stopped just like one that
    /// panics.
    #[tokio::test(start_paused = true)]
    async fn restarts_back_off_up_to_the_cap() {
        let health = Arc::new(WorkerHealth::default());
        let starts = Arc::new(Mutex::new(Vec::new()));
        let recorded = starts.clone();
        let supervisor = tokio::spawn(supervise(
            health.clone(),
            "memory_sweep",
            policy(),
            move || {
                recorded.lock().unwrap().push(tokio::time::Instant::now());
                async {}
            },
        ));

        tokio::time::sleep(Duration::from_secs(120)).await;
        supervisor.abort();

        let starts = starts.lock().unwrap().clone();
        let gaps: Vec<Duration> = starts
            .windows(2)
            .map(|pair| pair[1].duration_since(pair[0]))
            .collect();
        assert!(gaps.len() >= 10, "restarts: {gaps:?}");
        assert!(gaps.iter().all(|gap| *gap <= policy().cap), "{gaps:?}");
        assert!(
            gaps.last().unwrap() >= &(policy().cap / 2),
            "a worker that keeps stopping reaches the cap: {gaps:?}"
        );
        assert!(gaps[0] < gaps[gaps.len() - 1], "{gaps:?}");
        // Every start stopped at once, the last one included.
        let sweep = worker(&health, "memory_sweep");
        assert_eq!(sweep.last_error.as_deref(), Some("the worker returned"));
        assert_eq!(sweep.restarts, u64::try_from(starts.len()).unwrap());
    }

    /// During shutdown a worker that stops is not started again, and the
    /// snapshot says it stopped.
    #[tokio::test(start_paused = true)]
    async fn a_worker_that_stops_during_shutdown_stays_stopped() {
        let health = Arc::new(WorkerHealth::default());
        let starts = Arc::new(AtomicUsize::new(0));
        let counted = starts.clone();
        let release = Arc::new(tokio::sync::Notify::new());
        let released = release.clone();
        let supervisor = tokio::spawn(supervise(
            health.clone(),
            "mcp_supervisor",
            policy(),
            move || {
                counted.fetch_add(1, Ordering::SeqCst);
                let released = released.clone();
                async move { released.notified().await }
            },
        ));
        tokio::time::sleep(Duration::from_millis(10)).await;

        health.begin_shutdown();
        release.notify_one();
        supervisor.await.unwrap();

        assert_eq!(starts.load(Ordering::SeqCst), 1);
        let mcp = worker(&health, "mcp_supervisor");
        assert_eq!(mcp.state, WorkerState::Stopped);
        assert_eq!(mcp.restarts, 0);
    }

    #[test]
    fn a_long_error_is_bounded_and_kept_on_one_line() {
        let error = format!("first\nsecond {}", "x".repeat(MAX_ERROR_CHARS * 2));
        let kept = bounded(&error);
        assert!(!kept.contains('\n'));
        assert_eq!(kept.chars().count(), MAX_ERROR_CHARS + 1);
        assert!(kept.ends_with('…'));
    }

    /// The last error reaches the diagnostics export, so a panic message that
    /// carried a URL query or a token leaves both behind.
    #[test]
    fn a_stop_reason_is_scrubbed_before_the_snapshot_keeps_it() {
        // Assembled at run time, so no source line holds a token.
        let github_token = ["ghp", "0123456789abcdef"].join("_");
        let kept = bounded(&format!(
            "the worker panicked: GET https://gateway.example/v1?code=abc failed for {github_token}"
        ));
        assert_eq!(
            kept,
            "the worker panicked: GET https://gateway.example/v1?[redacted] failed for [redacted]"
        );
    }
}
