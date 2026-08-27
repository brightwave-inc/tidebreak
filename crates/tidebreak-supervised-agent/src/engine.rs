//! The engine seam the turn loop drives.
//!
//! The driver never sees a process. It sees an [`Engine`] that starts turns
//! and a [`TurnHandle`] it can wait on, steer, or interrupt. The engine-drive
//! slice implements this over `tidebreak-harness`; the tests here implement
//! it over channels. The seam exists so the turn state machine — the part
//! with the ordering bugs — is testable without an engine CLI on the path.

use async_trait::async_trait;

/// Where a turn's input came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnSource {
    /// The spawn task, on the first turn of the first incarnation.
    SpawnTask,
    /// Steering messages drained from the inbox.
    Inbox,
    /// A goal-mode continuation with no new input.
    GoalResume,
}

/// One turn the driver asks the engine to run.
#[derive(Clone, Debug)]
pub struct TurnRequest {
    /// Turn number, counted across incarnations.
    pub turn: u32,
    /// Where the input came from.
    pub source: TurnSource,
    /// The input text.
    pub input: String,
}

/// How a turn ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnEnd {
    /// The engine ran the turn to its own end.
    Completed {
        /// Whether the engine reported success.
        success: bool,
    },
    /// The driver interrupted the turn.
    Interrupted,
    /// The engine died in a way the loop cannot recover from.
    Fatal {
        /// What went wrong.
        message: String,
    },
}

/// The engine could not start a turn.
#[derive(Debug, thiserror::Error)]
#[error("the engine could not start a turn: {message}")]
pub struct EngineError {
    /// What went wrong.
    pub message: String,
}

/// The running turn cannot take a mid-turn message.
///
/// Not an error: some engines only accept input between turns. The driver
/// keeps the message queued for the next turn.
#[derive(Debug, thiserror::Error)]
#[error("the engine cannot be steered mid-turn")]
pub struct SteerRefused;

/// What happened when the driver tried to steer a running turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SteerOutcome {
    /// The message reached the running engine.
    Delivered,
    /// The engine accepts input only between turns.
    Refused,
    /// The turn ended before the message reached it.
    Ended(TurnEnd),
}

/// A running turn.
#[async_trait]
pub trait TurnHandle: Send {
    /// Waits for the turn to end.
    ///
    /// Must be cancel-safe: the driver races this against its poll tick in a
    /// `select` loop and re-calls it after every tick, so a cancelled call
    /// must neither lose the outcome nor corrupt the handle.
    async fn wait(&mut self) -> TurnEnd;

    /// Delivers a message into the running turn.
    ///
    /// Completion must win when it races delivery. Returning
    /// [`SteerOutcome::Ended`] keeps the message unacknowledged so the next
    /// turn receives it.
    async fn steer(&mut self, body: String) -> SteerOutcome;

    /// Stops the turn. The next [`TurnHandle::wait`] reports how it ended —
    /// usually [`TurnEnd::Interrupted`], but a turn that finished first keeps
    /// its own outcome.
    async fn interrupt(&mut self);
}

/// An engine CLI the driver runs turns against.
#[async_trait]
pub trait Engine: Send {
    /// Starts one turn.
    async fn start_turn(
        &mut self,
        request: TurnRequest,
    ) -> Result<Box<dyn TurnHandle>, EngineError>;
}
