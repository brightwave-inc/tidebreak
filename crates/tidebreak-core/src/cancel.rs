//! Cooperative turn cancellation.
//!
//! A [`CancelToken`] is the signal a running turn watches to stop early — the
//! server hands one to each turn and trips it when the client asks to cancel
//! (`POST /chats/{id}/cancel`). The agent loop checks it between steps and races
//! it against the long waits (the provider stream, a parked approval), so a
//! cancel takes effect promptly rather than only at the next natural boundary.
//!
//! Deliberately dependency-free: an [`AtomicBool`] for the level-triggered
//! `is_cancelled` check plus a small waker set for the awaitable
//! [`cancelled`](CancelToken::cancelled), so `tidebreak-core` needn't pull in a
//! runtime for this. One turn may have several independent read-only tools in
//! flight, so cancellation must wake every waiter rather than just one.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

/// A shared, cloneable signal that a turn should stop.
///
/// Clones share one underlying flag, so cancelling any handle cancels them all.
/// The default token is never cancelled — the agent uses it when no client-facing
/// cancellation is wired, so `run_turn` behaves exactly as before.
#[derive(Clone, Default)]
pub struct CancelToken {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    cancelled: AtomicBool,
    wakers: Mutex<Vec<Waker>>,
}

impl CancelToken {
    /// A fresh, un-cancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Trip the token. Idempotent — calling it again is a no-op. Wakes every
    /// task parked on [`cancelled`](Self::cancelled).
    pub fn cancel(&self) {
        if self.inner.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        let wakers = std::mem::take(&mut *self.inner.wakers.lock().unwrap());
        for waker in wakers {
            waker.wake();
        }
    }

    /// Whether the token has been cancelled. Cheap; use between loop steps.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    /// A future that resolves once the token is cancelled. Race it against a
    /// long wait (e.g. `select`ed against the provider stream) to preempt it.
    #[must_use]
    pub fn cancelled(&self) -> Cancelled {
        Cancelled {
            inner: Arc::clone(&self.inner),
        }
    }
}

/// The future returned by [`CancelToken::cancelled`]. `Unpin`, so it can be
/// dropped into [`futures::future::select`] without pinning.
pub struct Cancelled {
    inner: Arc<Inner>,
}

impl Future for Cancelled {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.inner.cancelled.load(Ordering::Acquire) {
            return Poll::Ready(());
        }
        // Hold the registration lock across the second check so a cancel racing
        // this poll either sees this waker or leaves the flag set for us to
        // observe. Each concurrent tool gets its own wake-up.
        let mut wakers = self.inner.wakers.lock().unwrap();
        if self.inner.cancelled.load(Ordering::Acquire) {
            Poll::Ready(())
        } else {
            if !wakers.iter().any(|waker| waker.will_wake(cx.waker())) {
                wakers.push(cx.waker().clone());
            }
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_future_resolves_when_tripped() {
        let token = CancelToken::new();
        let waiter = token.cancelled();
        token.cancel();
        // Already tripped, so this returns immediately rather than hanging.
        waiter.await;
    }

    #[tokio::test]
    async fn cancelled_future_wakes_every_parked_waiter() {
        let token = CancelToken::new();
        let first = token.clone();
        let second = token.clone();
        let first_waiter = tokio::spawn(async move { first.cancelled().await });
        let second_waiter = tokio::spawn(async move { second.cancelled().await });

        // Let both spawned tasks register before broadcasting the cancel.
        tokio::task::yield_now().await;
        token.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            first_waiter.await.unwrap();
            second_waiter.await.unwrap();
        })
        .await
        .expect("cancellation wakes every waiter");
    }
}
