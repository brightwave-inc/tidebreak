//! Host-owned Chrome computer use for coding harnesses.
//!
//! The three adapters (in-app browser, native apps, Chrome) share one
//! `computer_session` wire. This module provides the Chrome half: a direct
//! CDP driver and a dispatch service that binds owner/session/connection/tab/
//! origin, validates every call, rechecks grants at act time, and returns
//! request-id outcomes through the shared transport types.

pub mod cdp;
pub mod driver;
pub mod runtime;
#[cfg(test)]
mod tests;

pub use runtime::{
    ChromeAdapterState, ChromeCallOutcome, ChromeComputerUseService, ChromeConnectionSpec,
    ChromeOwnership, ChromeScope,
};
