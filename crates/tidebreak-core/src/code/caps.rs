//! Honest capability flags for an external agent engine.
//!
//! Every flag is stated as [`CapLevel::Supported`], [`CapLevel::Unsupported`],
//! or [`CapLevel::Unknown`]. [`HarnessCaps`] has no [`Default`] so adding a
//! flag forces every adapter to answer.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Whether an adapter can honor a capability for a probed engine version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CapLevel {
    /// Verified against a captured stream or a documented flag on this version.
    Supported,
    /// Verified absent on this version.
    Unsupported,
    /// Not yet verified; the product must not pretend otherwise.
    Unknown,
}

impl CapLevel {
    /// Stable database and wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
        }
    }
}

/// Adapter maturity, independent of any one capability flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum HarnessTier {
    /// Reference engine: every feature must work here.
    Reference,
    /// Second-line adapter.
    Secondary,
    /// Third-line adapter.
    Tertiary,
    /// Best-effort; honest `Unsupported` / `Unknown` is the default.
    BestEffort,
}

/// Capability vector for one probed engine version.
///
/// Constructed exhaustively — there is no [`Default`] — so a new flag is a
/// compile break at every adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HarnessCaps {
    /// The engine can resume a prior session from a native resume ref.
    pub resume: CapLevel,
    /// The engine streams partial assistant text.
    pub streaming_deltas: CapLevel,
    /// The engine exposes a structured approval channel.
    pub structured_approvals: CapLevel,
    /// The engine accepts a mid-turn user message.
    pub mid_turn_steering: CapLevel,
    /// The engine has a read-only / plan posture the adapter can select.
    pub plan_mode: CapLevel,
    /// The engine has a workspace-write / auto posture the adapter can select.
    ///
    /// Whether that posture is supervised is a separate question:
    /// with `structured_approvals` supported, sensitive actions still park on
    /// approval cards; without it, Auto runs unsupervised and the product
    /// states so where the mode is chosen (decision 0038).
    pub auto_mode: CapLevel,
    /// The engine accepts a reasoning-effort control.
    pub reasoning_levels: CapLevel,
    /// The engine emits native file-change events.
    pub native_file_change_events: CapLevel,
    /// The engine honors a native interrupt.
    pub native_interrupt: CapLevel,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_are_constructed_exhaustively() {
        // No Default: adding a field fails this (and every adapter) at compile time.
        let caps = HarnessCaps {
            resume: CapLevel::Supported,
            streaming_deltas: CapLevel::Supported,
            structured_approvals: CapLevel::Unknown,
            mid_turn_steering: CapLevel::Unknown,
            plan_mode: CapLevel::Supported,
            auto_mode: CapLevel::Unknown,
            reasoning_levels: CapLevel::Unknown,
            native_file_change_events: CapLevel::Unknown,
            native_interrupt: CapLevel::Supported,
        };
        assert_eq!(caps.resume, CapLevel::Supported);
        assert_eq!(caps.structured_approvals, CapLevel::Unknown);
    }
}
