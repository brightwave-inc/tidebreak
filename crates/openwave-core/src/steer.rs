//! Mid-turn steer — inject a user message into a running turn.
//!
//! A [`SteerInbox`] is the mailbox a running turn drains for steering input.
//! The server pushes via `POST /chats/{id}/steer`; the agent loop consumes at
//! step boundaries and (when `interrupt` is set) preempts the provider stream
//! the same way cancel does — except the turn **continues** after injecting the
//! message, rather than ending as [`TurnCancelled`](crate::AgentEvent::TurnCancelled).
//!
//! v1 scope: interrupt preempts the **model stream** only. A turn parked on
//! approval or mid-`execute` queues the steer until the next boundary (cancel
//! still unblocks approval). Cancel wins over steer when both are signalled.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures::task::AtomicWaker;

/// One steer message waiting to be injected into the running turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteerMessage {
    /// User text to append to the transcript.
    pub content: String,
}

/// Shared mailbox for mid-turn steering. Clones share one queue + interrupt flag.
#[derive(Clone, Default)]
pub struct SteerInbox {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    queue: Mutex<VecDeque<SteerMessage>>,
    interrupt: AtomicBool,
    waker: AtomicWaker,
}

impl SteerInbox {
    /// An empty inbox.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enqueue a steer message. When `interrupt` is true, also trip the interrupt
    /// flag so a turn racing the provider stream can preempt promptly.
    pub fn push(&self, content: impl Into<String>, interrupt: bool) {
        let content = content.into();
        if content.is_empty() {
            return;
        }
        self.inner
            .queue
            .lock()
            .unwrap()
            .push_back(SteerMessage { content });
        if interrupt {
            self.inner.interrupt.store(true, Ordering::Release);
            self.inner.waker.wake();
        }
    }

    /// Drain every queued message (order preserved). Clears the interrupt flag.
    pub fn drain(&self) -> Vec<SteerMessage> {
        let mut queue = self.inner.queue.lock().unwrap();
        let out: Vec<_> = queue.drain(..).collect();
        self.inner.interrupt.store(false, Ordering::Release);
        out
    }

    /// Whether an interrupt has been requested (level-triggered until [`drain`]).
    #[must_use]
    pub fn interrupt_requested(&self) -> bool {
        self.inner.interrupt.load(Ordering::Acquire)
    }

    /// A future that resolves once an interrupt is requested. Race it against the
    /// provider stream to preempt mid-step.
    #[must_use]
    pub fn interrupted(&self) -> Interrupted {
        Interrupted {
            inner: Arc::clone(&self.inner),
        }
    }
}

/// Future returned by [`SteerInbox::interrupted`].
pub struct Interrupted {
    inner: Arc<Inner>,
}

impl Future for Interrupted {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.inner.interrupt.load(Ordering::Acquire) {
            return Poll::Ready(());
        }
        self.inner.waker.register(cx.waker());
        if self.inner.interrupt.load(Ordering::Acquire) {
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
    fn push_and_drain_preserves_order() {
        let inbox = SteerInbox::new();
        inbox.push("one", false);
        inbox.push("two", false);
        let msgs = inbox.drain();
        assert_eq!(
            msgs.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(),
            vec!["one", "two"]
        );
        assert!(inbox.drain().is_empty());
    }

    #[test]
    fn empty_content_is_ignored() {
        let inbox = SteerInbox::new();
        inbox.push("", true);
        assert!(inbox.drain().is_empty());
        assert!(!inbox.interrupt_requested());
    }

    #[tokio::test]
    async fn interrupted_future_resolves_on_interrupt_push() {
        let inbox = SteerInbox::new();
        let waiter = inbox.interrupted();
        let clone = inbox.clone();
        let push = tokio::spawn(async move {
            clone.push("steer", true);
        });
        waiter.await;
        push.await.unwrap();
        assert_eq!(inbox.drain()[0].content, "steer");
    }
}
