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
    queue: Mutex<VecDeque<InboxItem>>,
    interrupt: AtomicBool,
    /// Set when the turn is about to emit `TurnCompleted`, so a concurrent
    /// [`SteerInbox::push`] cannot land a message that will never be drained.
    sealed: AtomicBool,
    waker: AtomicWaker,
}

enum InboxItem {
    Message(SteerMessage),
    DurableSignal,
}

impl SteerInbox {
    /// An empty inbox.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enqueue a steer message. When `interrupt` is true, also trip the interrupt
    /// flag so a turn racing the provider stream can preempt promptly.
    ///
    /// Returns `false` when the inbox has been sealed for turn completion (the
    /// message is not queued).
    pub fn push(&self, content: impl Into<String>, interrupt: bool) -> bool {
        let content = content.into();
        if content.is_empty() {
            return true;
        }
        let mut queue = self.inner.queue.lock().unwrap();
        if self.inner.sealed.load(Ordering::Acquire) {
            return false;
        }
        queue.push_back(InboxItem::Message(SteerMessage { content }));
        if interrupt {
            self.inner.interrupt.store(true, Ordering::Release);
            self.inner.waker.wake();
        }
        true
    }

    /// Wake a claimed turn after durable steering admission commits.
    ///
    /// The instruction content remains in the store and is applied under the
    /// worker's exact lease. This marker only supplies low-latency boundary or
    /// interrupt delivery; the database remains the source of truth.
    pub fn signal_durable(&self, interrupt: bool) -> bool {
        let mut queue = self.inner.queue.lock().unwrap();
        if self.inner.sealed.load(Ordering::Acquire) {
            return false;
        }
        if !queue
            .iter()
            .any(|item| matches!(item, InboxItem::DurableSignal))
        {
            queue.push_back(InboxItem::DurableSignal);
        }
        if interrupt {
            self.inner.interrupt.store(true, Ordering::Release);
            self.inner.waker.wake();
        }
        true
    }

    /// Drain every queued message (order preserved). Clears the interrupt flag.
    pub fn drain(&self) -> Vec<SteerMessage> {
        let mut queue = self.inner.queue.lock().unwrap();
        let out = queue
            .drain(..)
            .filter_map(|item| match item {
                InboxItem::Message(message) => Some(message),
                InboxItem::DurableSignal => None,
            })
            .collect();
        self.inner.interrupt.store(false, Ordering::Release);
        out
    }

    /// Whether an interrupt has been requested (level-triggered until [`drain`]).
    #[must_use]
    pub fn interrupt_requested(&self) -> bool {
        self.inner.interrupt.load(Ordering::Acquire)
    }

    /// Whether the inbox has no queued messages and no pending interrupt.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.queue.lock().unwrap().is_empty() && !self.interrupt_requested()
    }

    /// If the inbox is quiet, seal it and run `complete` so a concurrent
    /// [`push`](Self::push) cannot land between the empty check and the terminal
    /// event (which would 202 a steer that never gets applied).
    /// Returns `true` when `complete` ran; `false` when the inbox was not empty.
    pub fn try_complete<F: FnOnce()>(&self, complete: F) -> bool {
        let queue = self.inner.queue.lock().unwrap();
        if !queue.is_empty() || self.interrupt_requested() {
            return false;
        }
        self.inner.sealed.store(true, Ordering::Release);
        drop(queue);
        complete();
        true
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

    #[test]
    fn try_complete_seals_against_late_push() {
        let inbox = SteerInbox::new();
        assert!(inbox.try_complete(|| {}));
        assert!(!inbox.push("too late", true));
        assert!(inbox.drain().is_empty());
    }

    #[test]
    fn try_complete_refuses_when_queue_has_work() {
        let inbox = SteerInbox::new();
        assert!(inbox.push("pending", false));
        assert!(!inbox.try_complete(|| panic!("must not complete")));
        assert_eq!(inbox.drain()[0].content, "pending");
        assert!(inbox.push("after", false));
    }

    #[test]
    fn durable_signal_coalesces_and_fences_completion() {
        let inbox = SteerInbox::new();
        assert!(inbox.signal_durable(false));
        assert!(inbox.signal_durable(true));
        assert!(inbox.interrupt_requested());
        assert!(!inbox.try_complete(|| panic!("durable work is pending")));
        assert!(inbox.drain().is_empty());
        assert!(!inbox.interrupt_requested());
        assert!(inbox.try_complete(|| {}));
        assert!(!inbox.signal_durable(true));
    }

    #[tokio::test]
    async fn interrupted_future_resolves_on_interrupt_push() {
        let inbox = SteerInbox::new();
        let waiter = inbox.interrupted();
        let clone = inbox.clone();
        let push = tokio::spawn(async move {
            assert!(clone.push("steer", true));
        });
        waiter.await;
        push.await.unwrap();
        assert_eq!(inbox.drain()[0].content, "steer");
    }
}
