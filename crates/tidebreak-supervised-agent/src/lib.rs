//! The externally supervised Tidebreak agent (decision 0077).
//!
//! An externally supervised sandbox — a controlled execution environment that
//! Tidebreak does not provision — starts this agent, owns the durable event
//! stream, and exposes a control endpoint. The agent initiates outbound polls
//! to that endpoint, drives an engine CLI through `tidebreak-harness`, and
//! reports lifecycle events outward. It runs no listener, accepts no attach,
//! and keeps no durable state of its own: the endpoint's cursor is the truth.
//!
//! The crate ships one binary, `tidebreak-supervised-agent`, assembled from
//! modules that stay individually testable against an in-process mock
//! endpoint: the environment contract ([`inputs`]), trust and clone
//! preparation ([`trust`], [`bootstrap`]), the poll client ([`control`],
//! [`wire`]), the turn state machine ([`drive`]), and the engine seam
//! ([`engine`]) with its `tidebreak-harness` implementation
//! ([`harness_engine`]).

pub mod bootstrap;
pub mod completion;
pub mod control;
pub mod drive;
pub mod effort;
pub mod engine;
pub mod harness_engine;
pub mod inputs;
pub mod trust;
pub mod wip;
pub mod wire;

/// A required environment variable is absent or unusable.
pub const EXIT_MISSING_INPUT: i32 = 64;
/// The control endpoint refused this agent in a way retrying cannot fix.
pub const EXIT_CONTROL_FATAL: i32 = 69;
/// The engine failed in a way the turn loop cannot recover from.
pub const EXIT_ENGINE_FAILED: i32 = 70;
