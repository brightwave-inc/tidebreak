//! Bookkeeping for adapters that run one engine child per turn.
//!
//! Two facts the session worker needs, and a pid read at turn boundaries
//! cannot give it: the pid *while* the turn is in flight (that is the whole
//! window a crash can orphan a child in), and how the child ended once it is
//! gone (an EOF on stdout is not a completed turn).

use std::process::ExitStatus;

use tokio::sync::watch;

use crate::TurnOutcome;

/// Live pid of the child backing the current turn.
///
/// Every transition is published, so a watcher can persist the pid the moment
/// the child exists rather than discovering it after the turn is over.
#[derive(Debug)]
pub struct ChildPid {
    tx: watch::Sender<Option<i64>>,
}

impl Default for ChildPid {
    fn default() -> Self {
        Self::new()
    }
}

impl ChildPid {
    /// A cell with no child recorded.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tx: watch::Sender::new(None),
        }
    }

    /// Publish the pid of a child that now exists. An absent or zero pid is
    /// published as "no child": recovery must never probe an invented pid.
    pub fn set(&self, pid: Option<u32>) {
        let pid = pid.filter(|pid| *pid != 0).map(i64::from);
        self.tx.send_replace(pid);
    }

    /// Publish that no child is running.
    pub fn clear(&self) {
        self.tx.send_replace(None);
    }

    /// The pid recorded right now, if any.
    #[must_use]
    pub fn get(&self) -> Option<i64> {
        *self.tx.borrow()
    }

    /// Watch every transition, starting from the current value.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Option<i64>> {
        self.tx.subscribe()
    }
}

/// Ask a child this session spawned to stop.
///
/// SIGINT only: the caller decides whether to escalate after its own grace
/// period, and only ever against a pid recorded at spawn.
pub fn signal_interrupt(pid: i64) {
    #[cfg(unix)]
    {
        // SAFETY: the pid was recorded from a child we spawned this session.
        let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGINT) };
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

/// Captured stderr carried on an incomplete turn, in bytes.
const MAX_DETAIL_STDERR_BYTES: usize = 2 * 1_024;

/// Classify how the child that ran one turn ended.
///
/// `saw_terminal` is whether the stream reported a terminal turn event.
/// `status` is `None` when the exit could not be observed — a child another
/// task already reaped, for instance.
#[must_use]
pub fn turn_outcome(status: Option<ExitStatus>, saw_terminal: bool, stderr: &str) -> TurnOutcome {
    let failure = status.filter(|status| !status.success()).map(describe_exit);
    if failure.is_none() && saw_terminal {
        return TurnOutcome::Clean;
    }
    let mut detail =
        failure.unwrap_or_else(|| "the engine exited without reporting a result".to_owned());
    let tail = stderr_tail(stderr);
    if !tail.is_empty() {
        detail.push_str(": ");
        detail.push_str(tail);
    }
    TurnOutcome::Incomplete { detail }
}

fn describe_exit(status: ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("the engine was terminated by signal {signal}");
        }
    }
    match status.code() {
        Some(code) => format!("the engine exited with status {code}"),
        None => "the engine exited abnormally".to_owned(),
    }
}

/// The tail of captured stderr — the end carries the failure, the head
/// carries startup chatter.
fn stderr_tail(stderr: &str) -> &str {
    let trimmed = stderr.trim();
    if trimmed.len() <= MAX_DETAIL_STDERR_BYTES {
        return trimmed;
    }
    let mut start = trimmed.len() - MAX_DETAIL_STDERR_BYTES;
    while start < trimmed.len() && !trimmed.is_char_boundary(start) {
        start += 1;
    }
    &trimmed[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_that_stopped_without_a_result_is_incomplete() {
        // The defect this guards: EOF on stdout was read as a completed turn,
        // so a killed or crashed child journaled as success.
        let outcome = turn_outcome(None, false, "");
        assert!(matches!(outcome, TurnOutcome::Incomplete { .. }));
        assert_eq!(turn_outcome(None, true, ""), TurnOutcome::Clean);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_exit_is_incomplete_even_after_a_terminal_event_and_carries_stderr() {
        use std::os::unix::process::ExitStatusExt;
        let outcome = turn_outcome(Some(ExitStatus::from_raw(3 << 8)), true, "  boom  ");
        match outcome {
            TurnOutcome::Incomplete { detail } => {
                assert!(detail.contains("status 3"), "{detail}");
                assert!(detail.ends_with("boom"), "{detail}");
            }
            other => panic!("{other:?}"),
        }
    }
}
