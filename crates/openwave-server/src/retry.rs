//! Shared retry scheduling for the durable workers.
//!
//! Every durable worker parks a retryable failure for a while and eventually
//! gives up. The policy is the same everywhere — exponential waits, jitter so
//! items that failed together do not retry together, a ceiling on any single
//! wait, and a wall-clock envelope past which waiting longer cannot help. Only
//! the numbers differ, because a chat turn, a sandboxed background run, and a
//! blob deletion sweep have very different ideas of how long anyone is willing
//! to wait.

use std::time::Duration;

use chrono::{DateTime, Utc};

/// The durable retry state of one work item, as the schedule sees it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RetryAttempt {
    /// Row identity. Seeds the jitter so a given item's schedule is
    /// reproducible in a debugger and in tests.
    pub(crate) id: uuid::Uuid,
    /// Attempts already started, including the one that just failed.
    pub(crate) attempt_count: i32,
    /// Attempt budget after which the failure becomes terminal.
    pub(crate) max_attempts: i32,
    /// When this item's current episode began. The envelope is measured from
    /// here, not from the current failure.
    pub(crate) first_attempt_at: DateTime<Utc>,
}

/// When a retryably failed work item becomes claimable again.
///
/// The schedule is bounded by wall clock, not by attempt count: each attempt
/// waits twice as long as the last, and no attempt is scheduled past
/// `envelope` measured from the item's first claim. Work that cannot be
/// retried inside the envelope fails now with its own error rather than being
/// parked for longer than anyone is waiting.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RetrySchedule {
    /// Wait before the first retry, doubled for each attempt after it.
    initial: Duration,
    /// Ceiling on any single computed wait.
    max_delay: Duration,
    /// Wall-clock budget from the item's first claim to its last retry.
    envelope: Duration,
}

impl RetrySchedule {
    pub(crate) const fn new(initial: Duration, max_delay: Duration, envelope: Duration) -> Self {
        Self {
            initial,
            max_delay,
            envelope,
        }
    }

    /// The ceiling on any single wait, for the callers that must supply some
    /// delay even when the schedule has decided no further attempt is useful.
    pub(crate) const fn max_delay(self) -> Duration {
        self.max_delay
    }

    /// Decide when a retryable failure may be attempted again, or `None` when
    /// waiting can no longer help and the failure should be made permanent.
    pub(crate) fn next_attempt_at(
        self,
        attempt: RetryAttempt,
        hint: Option<Duration>,
        now: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        let delay = self.delay(attempt, hint, now)?;
        now.checked_add_signed(chrono::Duration::from_std(delay).ok()?)
    }

    /// The same decision expressed as a wait, for the durable transitions that
    /// take a delay and derive the timestamp from the database clock.
    ///
    /// `hint` is the provider's `Retry-After`. It wins over the computed
    /// backoff whenever it is longer, because retrying before a rate limit
    /// clears spends an attempt on a request that is already refused — the
    /// exact way a fixed one-second delay burns a whole budget in a few
    /// seconds. It is floored at `initial` so a `Retry-After: 0` cannot
    /// produce a hot loop.
    ///
    /// A hint longer than the remaining envelope is refused outright instead
    /// of being honored: parking the work for it would consume an attempt that
    /// the deadline will never let run.
    pub(crate) fn delay(
        self,
        attempt: RetryAttempt,
        hint: Option<Duration>,
        now: DateTime<Utc>,
    ) -> Option<Duration> {
        if attempt.attempt_count >= attempt.max_attempts {
            return None;
        }
        let deadline = attempt.first_attempt_at + chrono::Duration::from_std(self.envelope).ok()?;
        if now >= deadline {
            return None;
        }
        let delay = match hint {
            Some(hint) => hint.max(self.initial),
            None => jitter(
                self.backoff(attempt.attempt_count),
                attempt.id,
                attempt.attempt_count,
            ),
        };
        let at = now.checked_add_signed(chrono::Duration::from_std(delay).ok()?)?;
        (at <= deadline).then_some(delay)
    }

    /// Exponential wait for the attempt that just failed, capped at
    /// `max_delay`. Attempt one waits `initial`.
    fn backoff(self, attempt_count: i32) -> Duration {
        let doublings = u32::try_from(attempt_count.saturating_sub(1)).unwrap_or(0);
        self.initial
            .checked_mul(1_u32.checked_shl(doublings.min(31)).unwrap_or(u32::MAX))
            .unwrap_or(self.max_delay)
            .min(self.max_delay)
    }
}

/// Spread a computed wait over `[delay / 2, delay]`.
///
/// Items that failed together — the common case, since they failed against the
/// same provider or the same busy file — must not retry together. The offset
/// is derived from the item's own identity rather than a random source so a
/// given item's schedule is reproducible in a debugger and in tests.
fn jitter(delay: Duration, id: uuid::Uuid, attempt_count: i32) -> Duration {
    let seed = u64::from_le_bytes(id.as_bytes()[..8].try_into().unwrap_or([0; 8]))
        .wrapping_add(u64::from(attempt_count.unsigned_abs()));
    // splitmix64 finalizer: the raw UUID bytes are not uniformly spread across
    // the low bits we sample.
    let mut mixed = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^= mixed >> 31;
    let half = delay / 2;
    half + half.mul_f64(((mixed >> 11) as f64) / ((1_u64 << 53) as f64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(attempt_count: i32, first_attempt_at: DateTime<Utc>) -> RetryAttempt {
        RetryAttempt {
            id: uuid::Uuid::new_v4(),
            attempt_count,
            max_attempts: 4,
            first_attempt_at,
        }
    }

    /// The scheduling contract a rate-limited turn depends on: waits grow, the
    /// provider's own `Retry-After` beats a guess that is far too short, and
    /// neither the attempt budget nor a long hint can schedule work the
    /// wall-clock envelope will never let run.
    #[test]
    fn retry_waits_grow_and_defer_to_the_provider_inside_the_envelope() {
        let schedule = RetrySchedule::new(
            Duration::from_millis(250),
            Duration::from_secs(100),
            Duration::from_secs(600),
        );
        let start = Utc::now();

        // Exponential, jittered into the top half of each computed wait.
        for (attempt, base) in [(1, 250), (2, 500), (3, 1_000)] {
            let at = schedule
                .next_attempt_at(failed(attempt, start), None, start)
                .expect("a fresh item retries");
            let waited = (at - start).num_milliseconds();
            assert!(
                (base / 2..=base).contains(&waited),
                "attempt {attempt} waited {waited}ms, expected {}..={base}ms",
                base / 2
            );
        }

        // A 60-second rate-limit hint replaces the sub-second guess that used
        // to spend the whole budget before the limit cleared.
        let hinted = schedule
            .next_attempt_at(failed(1, start), Some(Duration::from_secs(60)), start)
            .expect("a hint inside the envelope is honored");
        assert_eq!((hinted - start).num_seconds(), 60);

        // A hint reaching past the envelope is refused outright rather than
        // spending an attempt on work the deadline will cancel.
        assert!(schedule
            .next_attempt_at(failed(1, start), Some(Duration::from_secs(900)), start)
            .is_none());

        // So is any retry once the envelope has elapsed, or once the durable
        // attempt budget is spent.
        let late = start + chrono::Duration::seconds(601);
        assert!(schedule
            .next_attempt_at(failed(1, start), None, late)
            .is_none());
        assert!(schedule
            .next_attempt_at(failed(4, start), None, start)
            .is_none());
    }
}
