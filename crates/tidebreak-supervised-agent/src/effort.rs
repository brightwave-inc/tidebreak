//! Reconciling the requested reasoning effort with the engine's ladder.
//!
//! The supervising environment forwards the spawn's requested effort as an
//! opaque token and expects the agent to report what actually applied. Its
//! published contract for first-party workloads is the one mirrored here: the
//! highest supported level at or below the request, and no explicit effort
//! control at all when the request sits below the engine's whole ladder.
//!
//! That last clause is why this module does not use
//! `ReasoningEffort::clamp_to`: `clamp_to` degrades a below-ladder request
//! *up* to the ladder's minimum so a stored chat setting keeps working, but
//! here rounding a request up would spend more reasoning than the caller
//! asked for. Reporting `None` and leaving the engine's default in force is
//! the honest answer.

use tidebreak_core::ReasoningEffort;

/// The effort level to drive the engine with, given the requested token and
/// the engine's supported ladder.
///
/// - No request, or a token no level spells: no explicit control.
/// - `minimal` reads as [`ReasoningEffort::None`] — the environment's ladder
///   has that extra rung below `low`, and "as little reasoning as possible"
///   is what both spell.
/// - An empty ladder means the engine declares no effort control; the parsed
///   level passes through untouched and the adapter applies it per model.
/// - Otherwise: the highest supported level at or below the request, or no
///   control when every supported level sits above it.
#[must_use]
pub fn reconcile(requested: Option<&str>, ladder: &[ReasoningEffort]) -> Option<ReasoningEffort> {
    let requested = parse(requested?)?;
    if ladder.is_empty() {
        return Some(requested);
    }
    ladder
        .iter()
        .copied()
        .filter(|level| *level <= requested)
        .max()
}

fn parse(token: &str) -> Option<ReasoningEffort> {
    if token == "minimal" {
        return Some(ReasoningEffort::None);
    }
    ReasoningEffort::from_str(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LADDER: &[ReasoningEffort] = &[
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
        ReasoningEffort::XHigh,
    ];

    #[test]
    fn an_exact_match_passes_through() {
        assert_eq!(reconcile(Some("high"), LADDER), Some(ReasoningEffort::High));
    }

    #[test]
    fn a_request_above_the_ladder_takes_the_highest_supported_level() {
        assert_eq!(
            reconcile(Some("ultra"), LADDER),
            Some(ReasoningEffort::XHigh)
        );
    }

    /// Rounding a below-ladder request up would spend more reasoning than the
    /// caller asked for; the answer is no explicit control.
    #[test]
    fn a_request_below_the_whole_ladder_applies_no_control() {
        assert_eq!(reconcile(Some("none"), LADDER), None);
        assert_eq!(reconcile(Some("minimal"), LADDER), None);
    }

    #[test]
    fn minimal_reads_as_none() {
        assert_eq!(
            reconcile(
                Some("minimal"),
                &[ReasoningEffort::None, ReasoningEffort::Low]
            ),
            Some(ReasoningEffort::None)
        );
    }

    #[test]
    fn an_empty_ladder_passes_the_parsed_level_through() {
        assert_eq!(reconcile(Some("xhigh"), &[]), Some(ReasoningEffort::XHigh));
    }

    #[test]
    fn no_request_and_unknown_tokens_apply_no_control() {
        assert_eq!(reconcile(None, LADDER), None);
        assert_eq!(reconcile(Some("turbo"), LADDER), None);
        assert_eq!(reconcile(Some("turbo"), &[]), None);
    }
}
