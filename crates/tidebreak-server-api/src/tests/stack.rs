//! The turn path's stack margin.
//!
//! Tokio's worker threads and the test harness's threads get 2 MiB of stack.
//! An unoptimized build keeps each awaited future in its caller's frame, so a
//! turn that nests a session worker, the turn driver, an engine, and the
//! engine's tools can run a thread out of it. The deepest turn tests run on
//! half of that through [`on_turn_stack`], so a frame that grows back fails
//! those tests instead of overflowing a release.

use std::future::Future;
use std::thread;

/// Half the stack a Tokio worker thread gets.
const TURN_STACK: usize = 1024 * 1024;

/// Run one turn test on a thread with a 1 MiB stack, in the current-thread
/// runtime `#[tokio::test]` would build.
///
/// An overflow aborts the process and names the thread, so the thread carries
/// the test's name.
pub(super) fn on_turn_stack<F, Fut>(test: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()>,
{
    let name = thread::current()
        .name()
        .map_or_else(|| "turn test".to_owned(), str::to_owned);
    let outcome = thread::Builder::new()
        .name(format!("{name} on a 1 MiB stack"))
        .stack_size(TURN_STACK)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build the test runtime")
                .block_on(test());
        })
        .expect("spawn the test thread")
        .join();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
