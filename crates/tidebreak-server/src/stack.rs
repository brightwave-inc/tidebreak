//! Keeping the turn path's stack frames small.
//!
//! An unoptimized build gives every future an `async fn` awaits its own slot
//! in the awaiting function's frame, and keeps that slot for as long as the
//! function runs. The turn path nests a session worker, the turn driver, an
//! engine, and the engine's tools, so those slots add up. They, not the heap,
//! are what runs a thread out of stack. Tokio's worker threads and the test
//! harness's threads both default to 2 MiB.
//!
//! [`boxed`] builds a future in its own short-lived frame and moves it to the
//! heap, so the caller keeps only a pointer. The turn tests run on a 1 MiB
//! stack (`tests::stack` in the server crate), so a frame that grows back
//! fails a test instead of a release.

use futures::future::BoxFuture;

/// Build the future `make` returns and box it, so the awaiting frame holds a
/// pointer rather than the future.
///
/// Use it at call boundaries on the turn path, where a large future would
/// otherwise stay in the caller's frame for the whole turn:
/// `boxed(|| drain_queued(&mut session, ..)).await`.
#[inline(never)]
pub(crate) fn boxed<'a, F, Fut>(make: F) -> BoxFuture<'a, Fut::Output>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future + Send + 'a,
{
    Box::pin(make())
}
