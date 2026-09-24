//! Adds conversations that predate the message index to it.
//!
//! Migration `m20260924_000004_message_search` queues every session that had
//! a turn when it ran. New messages are indexed as they are written, so this
//! worker only works through that queue, a few conversations at a time,
//! newest activity first, and stops once nothing is waiting. A conversation
//! that fails is tried again after a wait, and given up on after repeated
//! failures. Until the queue is empty the search route reports how many of the
//! caller's conversations remain, and how many could not be added.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tidebreak_core::DbStore;

/// Conversations one step rebuilds. Each is its own short transaction, so a
/// step never holds the writer for long, and turns keep moving in between.
const SESSIONS_PER_STEP: u64 = 16;

/// How long to wait after a step fails before trying again.
const RETRY_AFTER: Duration = Duration::from_secs(30);

/// Shortest wait for a conversation's next attempt, so a clock that reads a
/// due time as just past never spins the loop.
const MIN_WAIT: Duration = Duration::from_secs(1);

/// Work through the backfill queue until nothing is waiting, then return.
pub(crate) async fn run(db: Arc<DbStore>) {
    let started = Instant::now();
    let mut steps = 0_u64;
    loop {
        match db.backfill_message_search(SESSIONS_PER_STEP).await {
            Ok(state) if state.waiting == 0 => {
                if state.failed > 0 {
                    tracing::warn!(
                        failed = state.failed,
                        "tidebreak: some earlier conversations could not be added to the message search index"
                    );
                } else if steps > 0 {
                    tracing::info!(
                        elapsed_ms =
                            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                        "tidebreak: every conversation is in the message search index"
                    );
                }
                return;
            }
            Ok(state) => {
                steps += 1;
                if steps == 1 {
                    tracing::info!(
                        remaining = state.waiting,
                        "tidebreak: adding earlier conversations to the message search index"
                    );
                }
                match state.next_attempt_at {
                    // Something is due now: keep going, but let other work in.
                    None => tokio::task::yield_now().await,
                    // Everything left waits to try again after a failure.
                    Some(at) => {
                        let wait = (at - chrono::Utc::now())
                            .to_std()
                            .unwrap_or(Duration::ZERO)
                            .max(MIN_WAIT);
                        tokio::time::sleep(wait).await;
                    }
                }
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "tidebreak: could not add conversations to the message search index; retrying"
                );
                tokio::time::sleep(RETRY_AFTER).await;
            }
        }
    }
}
