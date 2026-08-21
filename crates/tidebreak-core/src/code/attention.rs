//! Attention for a unit of supervised work.
//!
//! The vocabulary is defined over "a unit of supervised work", not over a
//! coding session specifically, so chat can adopt it later without renaming.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Longest prompt stored on a `NeedsYou` state.
pub const MAX_ATTENTION_PROMPT: usize = 240;
/// Longest note stored on a `Manual` state.
pub const MAX_ATTENTION_NOTE: usize = 240;

/// Why the current attention state was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AttentionSource {
    /// An exact protocol event.
    Structured,
    /// An inference such as idle time.
    Heuristic,
    /// A state-machine fact (spawned, ended, recovered).
    Lifecycle,
    /// The user pinned or replaced the state by hand.
    User,
}

impl AttentionSource {
    /// Stable database and wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Structured => "structured",
            Self::Heuristic => "heuristic",
            Self::Lifecycle => "lifecycle",
            Self::User => "user",
        }
    }

    /// Parse a stored/wire token.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "structured" => Some(Self::Structured),
            "heuristic" => Some(Self::Heuristic),
            "lifecycle" => Some(Self::Lifecycle),
            "user" => Some(Self::User),
            _ => None,
        }
    }

    /// Automatic sources — everything except [`Self::User`].
    #[must_use]
    pub const fn is_automatic(self) -> bool {
        !matches!(self, Self::User)
    }
}

/// Why a session is fenced: observed but not controlled, until an explicit
/// user reap resolves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FenceReason {
    /// The recorded child pid is still alive; only an explicit reap resolves it.
    OrphanAlive,
    /// A pid-reuse or probe ambiguity; safer to fence than to signal.
    ProbeAmbiguous {
        /// Bounded human-readable detail.
        detail: String,
    },
    /// The engine no longer has the session this one resumed. The stored
    /// resume ref is dropped when this is set, so a reap starts a fresh
    /// engine session instead of resuming a ref that will fail again.
    ResumeLost {
        /// Bounded human-readable detail, as the engine reported it.
        detail: String,
    },
    /// Consecutive turns failed without one succeeding between them.
    ///
    /// Whatever broke is not specific to a prompt — an expired credential,
    /// a revoked key, an engine that cannot reach its provider — so the next
    /// turn would fail the same way. Fencing offers a reap instead of a
    /// session that reads idle and is not.
    RepeatedTurnFailures {
        /// How many turns failed in a row.
        count: u32,
        /// Bounded detail from the last failure, as the engine reported it.
        detail: String,
    },
}

impl FenceReason {
    /// Whether this fence must also stop every other session in the workspace.
    ///
    /// True when an engine process may be writing to the shared worktree
    /// outside every lock this process holds — an orphan from a previous
    /// boot, a pid the probe could not identify, or a session the engine no
    /// longer recognizes. Nothing in the workspace may write until a reap
    /// settles it (decision 0055).
    ///
    /// False for [`Self::RepeatedTurnFailures`]. That fence says the engine
    /// answered and the turns failed — an expired credential, a refused
    /// prompt, a provider outage. The process is accounted for and the
    /// worktree is not at risk, so a healthy sibling session keeps working.
    #[must_use]
    pub const fn blocks_workspace(&self) -> bool {
        match self {
            Self::OrphanAlive | Self::ProbeAmbiguous { .. } | Self::ResumeLost { .. } => true,
            Self::RepeatedTurnFailures { .. } => false,
        }
    }
}

/// Server-computed attention for one unit of supervised work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AttentionState {
    /// The engine is doing something.
    Working,
    /// An approval, question, or failure is waiting on the user.
    NeedsYou {
        /// Short prompt shown on the badge.
        prompt: String,
        /// How this need was detected.
        source: AttentionSource,
    },
    /// Running but silent past a threshold.
    Stalled {
        /// Seconds of observed silence.
        idle_secs: u32,
    },
    /// Finished work the user has not looked at.
    DoneUnreviewed,
    /// Crash recovery parked it.
    Fenced {
        /// Why it was fenced.
        reason: FenceReason,
    },
    /// The user pinned a state by hand.
    Manual {
        /// User-supplied note.
        note: String,
    },
}

/// An attention state together with the source that produced it.
///
/// [`AttentionState::NeedsYou`] also carries a source so a structured need
/// stays distinguishable after the pair is stored as JSON. The two sources
/// must agree when the state is `NeedsYou`; [`Attention::needs_you`] enforces
/// that at construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Attention {
    /// The state.
    pub state: AttentionState,
    /// Who or what set it.
    pub source: AttentionSource,
}

impl Attention {
    /// Build an attention value. For `NeedsYou`, `source` is taken from the
    /// variant so the pair cannot disagree.
    #[must_use]
    pub fn new(state: AttentionState, source: AttentionSource) -> Self {
        let source = match &state {
            AttentionState::NeedsYou {
                source: inner_source,
                ..
            } => *inner_source,
            AttentionState::Manual { .. } if source.is_automatic() => AttentionSource::User,
            _ => source,
        };
        Self { state, source }
    }

    /// A structured or otherwise sourced "needs you".
    #[must_use]
    pub fn needs_you(prompt: impl Into<String>, source: AttentionSource) -> Self {
        let prompt = truncate_chars(prompt.into(), MAX_ATTENTION_PROMPT);
        Self::new(AttentionState::NeedsYou { prompt, source }, source)
    }

    /// Working, from the given source.
    #[must_use]
    pub fn working(source: AttentionSource) -> Self {
        Self::new(AttentionState::Working, source)
    }

    /// A user-pinned note.
    #[must_use]
    pub fn manual(note: impl Into<String>) -> Self {
        let note = truncate_chars(note.into(), MAX_ATTENTION_NOTE);
        Self::new(AttentionState::Manual { note }, AttentionSource::User)
    }
}

/// Whether `next` may replace `current`.
///
/// A `Manual` state is never overwritten by any automatic source — only a
/// `User` source may replace it. A `Structured` `NeedsYou` is never
/// downgraded by a `Heuristic` source.
#[must_use]
pub fn should_replace(current: &Attention, next: &Attention) -> bool {
    if matches!(current.state, AttentionState::Manual { .. }) {
        return next.source == AttentionSource::User;
    }
    let current_is_structured_need =
        matches!(
            &current.state,
            AttentionState::NeedsYou {
                source: AttentionSource::Structured,
                ..
            }
        ) || (matches!(current.state, AttentionState::NeedsYou { .. })
            && current.source == AttentionSource::Structured);
    if current_is_structured_need && next.source == AttentionSource::Heuristic {
        return false;
    }
    true
}

fn truncate_chars(mut value: String, max: usize) -> String {
    if value.chars().count() > max {
        value = value.chars().take(max).collect();
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auto_working() -> Attention {
        Attention::working(AttentionSource::Lifecycle)
    }

    fn structured_need() -> Attention {
        Attention::needs_you("approve this", AttentionSource::Structured)
    }

    fn heuristic_stall() -> Attention {
        Attention::new(
            AttentionState::Stalled { idle_secs: 30 },
            AttentionSource::Heuristic,
        )
    }

    #[test]
    fn manual_is_never_replaced_by_automatic_sources() {
        let pinned = Attention::manual("look at this later");
        for source in [
            AttentionSource::Structured,
            AttentionSource::Heuristic,
            AttentionSource::Lifecycle,
        ] {
            let next = Attention::working(source);
            assert!(
                !should_replace(&pinned, &next),
                "{source:?} must not replace Manual"
            );
        }
        assert!(should_replace(&pinned, &Attention::manual("updated note")));
        assert!(should_replace(
            &pinned,
            &Attention::working(AttentionSource::User)
        ));
    }

    #[test]
    fn structured_needs_you_is_not_downgraded_by_heuristic() {
        let current = structured_need();
        assert!(!should_replace(&current, &heuristic_stall()));
        assert!(!should_replace(
            &current,
            &Attention::needs_you("maybe idle?", AttentionSource::Heuristic)
        ));
        assert!(should_replace(
            &current,
            &Attention::working(AttentionSource::Structured)
        ));
        assert!(should_replace(
            &current,
            &Attention::working(AttentionSource::Lifecycle)
        ));
        assert!(should_replace(
            &current,
            &Attention::working(AttentionSource::User)
        ));
    }

    #[test]
    fn automatic_transitions_otherwise_succeed() {
        assert!(should_replace(&auto_working(), &structured_need()));
        assert!(should_replace(&auto_working(), &heuristic_stall()));
        assert!(should_replace(
            &heuristic_stall(),
            &Attention::new(AttentionState::DoneUnreviewed, AttentionSource::Lifecycle)
        ));
    }

    #[test]
    fn no_automatic_sequence_clears_manual() {
        let start = Attention::manual("hold");
        let automatics = [
            Attention::working(AttentionSource::Lifecycle),
            Attention::working(AttentionSource::Structured),
            heuristic_stall(),
            structured_need(),
            Attention::new(AttentionState::DoneUnreviewed, AttentionSource::Lifecycle),
            Attention::new(
                AttentionState::Fenced {
                    reason: FenceReason::OrphanAlive,
                },
                AttentionSource::Lifecycle,
            ),
        ];
        for next in &automatics {
            assert!(
                !should_replace(&start, next),
                "automatic {:?} must not replace Manual",
                next.source
            );
        }
    }
}
