//! One permission vocabulary, for every conversation.
//!
//! Chat and code mode each carried their own enum with identical variants,
//! differing only in lifecycle: chat's was changeable per chat, code's was
//! fixed at session creation and refused per declared harness capability.
//! Decision 48 step 2 merges them, taking code's lifecycle as the default —
//! the mode is chosen when the conversation is created, and changing it
//! afterwards is a capability an engine declares rather than something every
//! engine is assumed to allow.
//!
//! The honesty rules from decisions 38 and 39 govern throughout: a mode an
//! engine cannot honor is refused, never approximated.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// How a conversation handles mutations and approvals.
///
/// For the internal engine the mode governs the server-side approval gate,
/// which is where all but one mutating call lives. Client-executed tools run
/// in the trusted desktop under their own consent — a folder grant the reader
/// picked, a card the native side raises — and do not re-enter that gate. The
/// one client call that mutates something the reader owns, publishing an
/// output into a connected folder, consults the mode itself: see
/// [`crate::OutputWriteMode::requires_user_decision`].
///
/// For an external agent engine each adapter maps these onto the engine's
/// native flags. A mode the engine cannot honor is refused at session
/// creation — never approximated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Read-only. The agent explores and designs a plan instead of acting.
    ///
    /// Mutating calls are refused outright — not parked on the approval card
    /// — so a plan turn cannot change anything no matter what the reader
    /// approves. An external adapter must select the engine's own plan or
    /// equivalent read-only mode.
    Plan,
    /// Every uncovered mutating call parks on an approval card. The default.
    ///
    /// For the internal engine that includes the one client-executed write
    /// that leaves the sandbox, and it is the only mode where
    /// `Workspace`-class tools ask. For an external engine it is whatever the
    /// engine itself classifies as needing approval; deny may carry feedback
    /// the engine surfaces to the model as the denial reason.
    Ask,
    /// Routine workspace writes proceed; sensitive calls still ask.
    ///
    /// A standing "yes" to workspace edits stated as a mode instead of a
    /// per-tool grant. Replacing an existing file in a connected folder still
    /// asks: no mode advertises consent for destroying bytes the agent did
    /// not write. An external adapter must select the engine's workspace-write
    /// posture, not a bypass flag.
    Auto,
    /// Nothing asks, with the same replacement exception `Auto` carries.
    ///
    /// An explicit per-conversation opt-in to full autonomy. For an external
    /// engine this is its allow-everything posture — its permission system
    /// off — and the adapter must compose the engine's documented bypass,
    /// never as a default and only when this mode is the explicit choice
    /// (decision 39).
    Allow,
}

impl PermissionMode {
    /// Every mode, in ascending order of autonomy.
    pub const ALL: &'static [Self] = &[Self::Plan, Self::Ask, Self::Auto, Self::Allow];

    /// The default for a new conversation: everything uncovered asks.
    pub const DEFAULT: Self = Self::Ask;

    /// The wire/storage token for this mode.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Ask => "ask",
            Self::Auto => "auto",
            Self::Allow => "allow",
        }
    }

    /// Parse a stored/wire token back into a mode.
    ///
    /// Deliberately returns `Option` (invalid tokens are dropped, not
    /// errored), so this is not the `FromStr` trait.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|mode| mode.as_str() == value)
    }
}

impl Default for PermissionMode {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl std::fmt::Display for PermissionMode {
    /// The same token `as_str` gives, so an error message and a stored value
    /// never disagree about what a mode is called.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tokens are what both surfaces already stored, so the merge is a
    /// type change and not a data change. A token drift here would silently
    /// orphan every persisted mode on one side or the other.
    #[test]
    fn the_merged_tokens_are_the_ones_both_surfaces_already_stored() {
        assert_eq!(
            PermissionMode::ALL
                .iter()
                .map(|mode| mode.as_str())
                .collect::<Vec<_>>(),
            ["plan", "ask", "auto", "allow"]
        );
        for mode in PermissionMode::ALL {
            assert_eq!(PermissionMode::from_str(mode.as_str()), Some(*mode));
        }
        assert_eq!(PermissionMode::from_str("bypass"), None);
        // The safe end of the ladder, not the autonomous one.
        assert_eq!(PermissionMode::default(), PermissionMode::Ask);
    }
}
