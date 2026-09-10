//! Host-owned Chrome computer use for coding harnesses.
//!
//! Native apps and Chrome share one `computer_session` wire; the in-app
//! browser keeps its own channel. This module provides the Chrome half: a
//! direct CDP driver and a dispatch service that binds
//! owner/session/connection/tab/origin, validates every call, clones the
//! grant once at connect and rechecks the fence at act time, and returns
//! request-id outcomes through the shared transport types.

pub mod cdp;
pub mod driver;
pub mod runtime;
#[cfg(test)]
mod tests;

pub use runtime::{
    validate_websocket_endpoint, ChromeAdapterState, ChromeCallOutcome, ChromeComputerUseService,
    ChromeConnectionSpec, ChromeDiscoveredTab, ChromeOwnership, ChromeScope,
};
