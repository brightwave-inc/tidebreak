//! The lane loop every durable worker runs.
//!
//! A worker lane claims one item, works it, and goes back for the next. The
//! scaffolding around that — how long to sleep when the queue is empty, how
//! to back off when the store is struggling, how a wake signal cuts a sleep
//! short, and how a lane that panics is put back — is the same for every
//! worker. It lives here once so a fix to lane pacing or restart is one edit,
//! and each worker keeps only its `run_once`.

use std::future::Future;
use std::time::Duration;

use tidebreak_core::Result;
use tokio::sync::Notify;

use crate::retry::LaneBackoff;

/// What one lane iteration did, as far as pacing is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaneStep {
    /// Nothing was claimable. The lane sleeps, doubling its wait each time
    /// until the cap, and a wake signal ends the sleep early.
    Idle,
    /// An item was worked. The lane goes straight back for the next one and
    /// its idle wait returns to the minimum.
    Worked,
}

/// A worker outcome the lane loop can pace on.
pub(crate) trait LaneOutcome {
    fn lane_step(&self) -> LaneStep;
}

impl LaneOutcome for bool {
    /// `true` means an item was worked.
    fn lane_step(&self) -> LaneStep {
        if *self {
            LaneStep::Worked
        } else {
            LaneStep::Idle
        }
    }
}

impl LaneOutcome for LaneStep {
    fn lane_step(&self) -> LaneStep {
        *self
    }
}

/// How long a lane waits after an iteration error.
#[derive(Debug, Clone, Copy)]
pub(crate) enum FailureWait {
    /// The same wait after every failure.
    Fixed(Duration),
    /// [`LaneBackoff`]: doubled per consecutive failure up to `cap`, jittered,
    /// and reset by one successful iteration.
    Backoff { initial: Duration, cap: Duration },
}

/// The sleeps a lane takes between iterations.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LanePacing {
    /// First idle wait, and the wait after any worked item.
    pub(crate) idle_min: Duration,
    /// Ceiling on the doubled idle wait.
    pub(crate) idle_cap: Duration,
    pub(crate) failure: FailureWait,
}

impl LanePacing {
    /// Pacing whose failure wait is a [`LaneBackoff`] between `failure_delay`
    /// and `failure_delay_cap` — the shape every durable worker config has.
    pub(crate) const fn backoff(
        idle_min: Duration,
        idle_cap: Duration,
        failure_delay: Duration,
        failure_delay_cap: Duration,
    ) -> Self {
        Self {
            idle_min,
            idle_cap,
            failure: FailureWait::Backoff {
                initial: failure_delay,
                cap: failure_delay_cap,
            },
        }
    }
}

/// The idle wait of one lane: starts at the minimum, doubles per idle
/// iteration up to the cap, and returns to the minimum whenever work appears.
#[derive(Debug, Clone)]
pub(crate) struct IdleDelay {
    current: Duration,
    min: Duration,
    cap: Duration,
}

impl IdleDelay {
    pub(crate) fn new(min: Duration, cap: Duration) -> Self {
        Self {
            current: min,
            min,
            cap,
        }
    }

    /// The wait for the next idle sleep.
    pub(crate) fn current(&self) -> Duration {
        self.current
    }

    /// Work appeared: the next idle sleep is the minimum again.
    pub(crate) fn reset(&mut self) {
        self.current = self.min;
    }

    /// Another idle iteration: wait twice as long next time, up to the cap.
    pub(crate) fn grow(&mut self) {
        self.current = self.current.saturating_mul(2).min(self.cap);
    }
}

enum FailureState {
    Fixed(Duration),
    Backoff(LaneBackoff),
}

impl FailureState {
    fn new(wait: FailureWait) -> Self {
        match wait {
            FailureWait::Fixed(delay) => Self::Fixed(delay),
            FailureWait::Backoff { initial, cap } => Self::Backoff(LaneBackoff::new(initial, cap)),
        }
    }

    fn reset(&mut self) {
        if let Self::Backoff(backoff) = self {
            backoff.reset();
        }
    }

    fn next_delay(&mut self) -> Duration {
        match self {
            Self::Fixed(delay) => *delay,
            Self::Backoff(backoff) => backoff.next_delay(),
        }
    }
}

/// Run one lane forever.
///
/// `run_once` claims and works at most one item. An idle iteration sleeps
/// the current idle wait, then doubles it; a worked item resets the idle wait
/// and the failure backoff; an error logs under `name` and sleeps the failure
/// wait. Every sleep ends early when `wake` is notified, so a producer that
/// just enqueued work never waits out a lane's idle cap.
pub(crate) async fn run_lane<O, F, Fut>(
    name: &'static str,
    pacing: LanePacing,
    wake: &Notify,
    mut run_once: F,
) where
    O: LaneOutcome,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<O>>,
{
    let mut idle = IdleDelay::new(pacing.idle_min, pacing.idle_cap);
    let mut failure = FailureState::new(pacing.failure);
    loop {
        match run_once().await {
            Ok(outcome) => match outcome.lane_step() {
                LaneStep::Idle => {
                    failure.reset();
                    tokio::select! {
                        _ = tokio::time::sleep(idle.current()) => {}
                        _ = wake.notified() => {}
                    }
                    idle.grow();
                }
                LaneStep::Worked => {
                    failure.reset();
                    idle.reset();
                }
            },
            Err(error) => {
                tracing::warn!("tidebreak: {name} iteration failed: {error}");
                let delay = failure.next_delay();
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = wake.notified() => {}
                }
            }
        }
    }
}

/// Keep `count` lanes running until the caller drops this future.
///
/// A lane only ever returns by panicking, since [`run_lane`] loops forever.
/// The panic is logged under `name` and the lane respawned after `restart`'s
/// next wait, so a lane that panics on every iteration does not spin. Dropping
/// the future drops the [`tokio::task::JoinSet`] and aborts every lane rather
/// than detaching background work.
pub(crate) async fn supervise_lanes<F, Fut>(
    name: &'static str,
    count: usize,
    mut restart: LaneBackoff,
    mut lane: F,
) where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut lanes = tokio::task::JoinSet::new();
    for _ in 0..count {
        lanes.spawn(lane());
    }
    while let Some(result) = lanes.join_next().await {
        if let Err(error) = result {
            tracing::error!("tidebreak: {name} lane stopped: {error}");
            tokio::time::sleep(restart.next_delay()).await;
        }
        lanes.spawn(lane());
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::Arc;

    use super::*;

    /// The pacing contract the workers lost their own copies of: an idle lane
    /// doubles its wait up to the cap, one worked item drops it back to the
    /// minimum, and a wake ends an idle sleep at once instead of after the
    /// current wait.
    #[tokio::test(start_paused = true)]
    async fn idle_wait_doubles_resets_on_work_and_yields_to_wake() {
        let pacing = LanePacing::backoff(
            Duration::from_millis(100),
            Duration::from_millis(400),
            Duration::from_secs(1),
            Duration::from_secs(1),
        );
        let wake = Arc::new(Notify::new());
        // Idle five times (100, 200, 400, 400, then a woken sleep), work
        // once, idle twice more, then hang so the loop stops iterating.
        let script = RefCell::new(
            vec![
                LaneStep::Idle,
                LaneStep::Idle,
                LaneStep::Idle,
                LaneStep::Idle,
                LaneStep::Idle,
                LaneStep::Worked,
                LaneStep::Idle,
                LaneStep::Idle,
            ]
            .into_iter(),
        );
        let started = tokio::time::Instant::now();
        let calls = RefCell::new(Vec::new());
        let waker = wake.clone();
        let lane = run_lane("test worker", pacing, &wake, || {
            let step = script.borrow_mut().next();
            calls.borrow_mut().push(started.elapsed());
            // Arm the wake while the fifth idle sleep is pending, so that
            // sleep ends at once instead of after its 400ms wait.
            if calls.borrow().len() == 5 {
                waker.notify_one();
            }
            async move {
                match step {
                    Some(step) => Ok(step),
                    None => std::future::pending().await,
                }
            }
        });
        let _ = tokio::time::timeout(Duration::from_secs(10), lane).await;

        let ms: Vec<u128> = calls.borrow().iter().map(|d| d.as_millis()).collect();
        // 0 → +100 → +200 → +400 (cap) → +400 → woken at once → worked, no
        // sleep → +100 (reset) → +200.
        assert_eq!(ms, vec![0, 100, 300, 700, 1100, 1100, 1100, 1200, 1400]);
    }
}
