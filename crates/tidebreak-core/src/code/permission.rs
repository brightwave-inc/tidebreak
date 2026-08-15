//! Permission modes for an external agent-engine session.
//!
//! Variant names match the chat product's [`crate::model::PermissionMode`]
//! vocabulary (`Plan`, `Ask`, `Auto`) so a future unification is a merge, not
//! a translation. There is no `Allow` here: a session that needs bypass
//! behavior must choose it explicitly at the product layer, never as a
//! composed default.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// How an external agent-engine session handles mutations and approvals.
///
/// Each adapter maps these onto the engine's native flags. A mode the
/// engine cannot honor is refused at session creation — never approximated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodePermissionMode {
    /// Read-only / plan posture. Mutations are refused by the engine; the
    /// adapter must select the engine's plan or equivalent read-only mode.
    /// A session in this mode must not be able to change the worktree no
    /// matter what the user approves.
    Plan,
    /// Every request that the engine classifies as needing approval parks on
    /// a card. This is the default. Deny may carry feedback that the engine
    /// surfaces to the model as the denial reason.
    Ask,
    /// Routine workspace writes proceed under the engine's own policy;
    /// sensitive actions still escalate to approval. The adapter must select
    /// the engine's workspace-write posture, not a bypass flag.
    Auto,
}

impl CodePermissionMode {
    /// Every mode, in ascending order of autonomy.
    pub const ALL: &'static [Self] = &[Self::Plan, Self::Ask, Self::Auto];

    /// The default for a new session.
    pub const DEFAULT: Self = Self::Ask;

    /// Stable database and wire token. Shared with chat's mode names.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Ask => "ask",
            Self::Auto => "auto",
        }
    }

    /// Parse a stored/wire token.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|mode| mode.as_str() == value)
    }
}

impl Default for CodePermissionMode {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl std::fmt::Display for CodePermissionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PermissionMode;

    #[test]
    fn names_are_a_subset_of_the_chat_vocabulary() {
        for mode in CodePermissionMode::ALL {
            assert!(
                PermissionMode::from_str(mode.as_str()).is_some(),
                "{} must share a token with chat PermissionMode",
                mode.as_str()
            );
        }
        assert_eq!(
            CodePermissionMode::Plan.as_str(),
            PermissionMode::Plan.as_str()
        );
        assert_eq!(
            CodePermissionMode::Ask.as_str(),
            PermissionMode::Ask.as_str()
        );
        assert_eq!(
            CodePermissionMode::Auto.as_str(),
            PermissionMode::Auto.as_str()
        );
    }
}
