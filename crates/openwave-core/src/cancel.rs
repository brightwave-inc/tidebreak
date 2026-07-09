//! Cooperative turn cancellation.
//!
//! A [`CancelToken`] is the signal a running turn watches to stop early — the
//! server hands one to each turn and trips it when the client asks to cancel
//! (`POST /chats/{id}/cancel`). The agent loop checks it between steps and races
//! it against the long waits (the provider stream, a parked approval), so a
//! cancel takes effect promptly rather than only at the next natural boundary.
//!
//! Deliberately dependency-free: an [`AtomicBool`] for the level-triggered
//! `is_cancelled` check plus a single [`AtomicWaker`] for the awaitable
//! [`cancelled`](CancelToken::cancelled), so `openwave-core` needn't pull in a
//! runtime for this. It is **single-consumer**: one turn task awaits a token at
//! a time (the loop never races the token from two places at once).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::task::AtomicWaker;

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
    waker: AtomicWaker,
}

impl CancelToken {
    /// A fresh, un-cancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Trip the token. Idempotent — calling it again is a no-op. Wakes a task
    /// parked on [`cancelled`](Self::cancelled).
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Release);
        self.inner.waker.wake();
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
        // Register before the second check so a cancel racing this poll can't
        // slip between the load and the park without waking us.
        self.inner.waker.register(cx.waker());
        if self.inner.cancelled.load(Ordering::Acquire) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_token_is_not_cancelled() {
        assert!(!CancelToken::new().is_cancelled());
    }

    #[test]
    fn clones_share_the_flag() {
        let token = CancelToken::new();
        let clone = token.clone();
        token.cancel();
        assert!(clone.is_cancelled());
    }

    #[tokio::test]
    async fn cancelled_future_resolves_when_tripped() {
        let token = CancelToken::new();
        let waiter = token.cancelled();
        token.cancel();
        // Already tripped, so this returns immediately rather than hanging.
        waiter.await;
    }

    #[tokio::test]
    async fn cancelled_future_wakes_a_parked_waiter() {
        let token = CancelToken::new();
        let waiter = token.cancelled();
        let clone = token.clone();
        // Trip the token from another task while the waiter is parked.
        let trip = tokio::spawn(async move {
            clone.cancel();
        });
        waiter.await;
        trip.await.unwrap();
    }
}
