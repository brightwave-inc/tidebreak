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
//!
//! The ladder itself comes from [`ladder`]: the adapter's engine-wide ladder
//! when it states one, otherwise the selected model's row from the engine's
//! catalog. Codex is why the second step exists — its ladder is per model,
//! and a token the selected row does not list makes the CLI refuse the turn
//! instead of running it.

use tidebreak_core::ReasoningEffort;
use tidebreak_harness::{HarnessAdapter, HarnessProbe};

/// The effort level to drive the engine with, given the requested token and
/// the supported ladder from [`ladder`].
///
/// - No request, or a token no level spells: no explicit control.
/// - `minimal` reads as [`ReasoningEffort::None`] — the environment's ladder
///   has that extra rung below `low`, and "as little reasoning as possible"
///   is what both spell.
/// - An empty ladder means no effort control is known to apply; the request
///   is dropped rather than passed through, because a token the engine never
///   advertised is one it may refuse.
/// - Otherwise: the highest supported level at or below the request, or no
///   control when every supported level sits above it.
#[must_use]
pub fn reconcile(requested: Option<&str>, ladder: &[ReasoningEffort]) -> Option<ReasoningEffort> {
    let requested = parse(requested?)?;
    ladder
        .iter()
        .copied()
        .filter(|level| *level <= requested)
        .max()
}

/// The ladder to reconcile the requested effort against.
///
/// The adapter's engine-wide ladder wins when it states one. When it does
/// not, the ladder is the selected model's row in the engine's catalog — the
/// named model's, or the default row's when no model was named. A model the
/// catalog does not list resolves to no ladder, so [`reconcile`] applies no
/// explicit control rather than sending a token the engine would reject.
pub async fn ladder(
    adapter: &dyn HarnessAdapter,
    probe: &HarnessProbe,
    model: Option<&str>,
) -> Vec<ReasoningEffort> {
    let engine_wide = adapter.reasoning_efforts(probe);
    if !engine_wide.is_empty() {
        return engine_wide;
    }
    adapter
        .list_models(probe)
        .await
        .into_iter()
        .find(|row| match model {
            Some(id) => row.id == id,
            None => row.default,
        })
        .map(|row| row.reasoning_efforts)
        .unwrap_or_default()
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

    use std::ffi::OsString;
    use std::path::PathBuf;

    use async_trait::async_trait;
    use tidebreak_core::{CapLevel, HarnessCaps, HarnessKind};
    use tidebreak_harness::{
        HarnessError, HarnessSession, HostEnv, ListedHarnessModel, SessionSpec,
    };

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

    /// A token the engine never advertised is one it may refuse: Codex
    /// validates the effort on `turn/start` against the selected model row,
    /// so an unresolved ladder must drop the request, not pass it through.
    #[test]
    fn an_empty_ladder_applies_no_control() {
        assert_eq!(reconcile(Some("xhigh"), &[]), None);
    }

    #[test]
    fn no_request_and_unknown_tokens_apply_no_control() {
        assert_eq!(reconcile(None, LADDER), None);
        assert_eq!(reconcile(Some("turbo"), LADDER), None);
        assert_eq!(reconcile(Some("turbo"), &[]), None);
    }

    /// Adapter with no engine-wide ladder and a fixed catalog — the Codex
    /// shape, where every row states its own ladder.
    struct PerModelAdapter;

    #[async_trait]
    impl HarnessAdapter for PerModelAdapter {
        fn kind(&self) -> HarnessKind {
            HarnessKind::Codex
        }

        async fn probe(&self, _host: &HostEnv) -> HarnessProbe {
            unreachable!("the tests build the probe directly")
        }

        fn capabilities(&self, _probe: &HarnessProbe) -> HarnessCaps {
            HarnessCaps {
                resume: CapLevel::Unknown,
                streaming_deltas: CapLevel::Unknown,
                structured_approvals: CapLevel::Unknown,
                mid_turn_steering: CapLevel::Unknown,
                plan_mode: CapLevel::Unknown,
                auto_mode: CapLevel::Unknown,
                allow_mode: CapLevel::Unknown,
                reasoning_levels: CapLevel::Unknown,
                native_file_change_events: CapLevel::Unknown,
                native_interrupt: CapLevel::Unknown,
                image_input: CapLevel::Unknown,
                slash_commands: CapLevel::Unknown,
                durable_parks: CapLevel::Unsupported,
                user_questions: CapLevel::Unsupported,
                standing_grants: CapLevel::Unsupported,
                memory_loopback: CapLevel::Unsupported,
            }
        }

        async fn list_models(&self, _probe: &HarnessProbe) -> Vec<ListedHarnessModel> {
            vec![
                ListedHarnessModel {
                    id: "wide-ladder".to_owned(),
                    label: "Wide ladder".to_owned(),
                    default: true,
                    reasoning_efforts: vec![
                        ReasoningEffort::Low,
                        ReasoningEffort::Medium,
                        ReasoningEffort::High,
                        ReasoningEffort::XHigh,
                    ],
                    fast_mode: false,
                },
                ListedHarnessModel {
                    id: "narrow-ladder".to_owned(),
                    label: "Narrow ladder".to_owned(),
                    default: false,
                    reasoning_efforts: vec![ReasoningEffort::Low, ReasoningEffort::Medium],
                    fast_mode: false,
                },
            ]
        }

        async fn launch(
            &self,
            _spec: SessionSpec,
        ) -> Result<Box<dyn HarnessSession>, HarnessError> {
            unreachable!("the tests never launch")
        }
    }

    fn bare_probe() -> HarnessProbe {
        HarnessProbe {
            found: true,
            binary_path: Some(PathBuf::from("/usr/bin/engine")),
            version: None,
            authenticated: None,
            stderr: String::new(),
            env: vec![(OsString::from("PATH"), OsString::from("/usr/bin"))],
            commands: Vec::new(),
        }
    }

    /// The finding this pins: a Codex spawn with a request above the model's
    /// ladder must degrade to that row's highest rung, exactly as first-party
    /// bootstrap clamps against the catalog, instead of shipping the raw
    /// token for the CLI to refuse.
    #[tokio::test]
    async fn a_per_model_ladder_clamps_the_request_to_the_selected_row() {
        let probe = bare_probe();
        let rungs = ladder(&PerModelAdapter, &probe, Some("narrow-ladder")).await;
        assert_eq!(
            reconcile(Some("ultra"), &rungs),
            Some(ReasoningEffort::Medium)
        );
    }

    #[tokio::test]
    async fn no_named_model_resolves_the_default_row() {
        let probe = bare_probe();
        let rungs = ladder(&PerModelAdapter, &probe, None).await;
        assert_eq!(rungs, LADDER);
    }

    #[tokio::test]
    async fn an_unlisted_model_applies_no_control() {
        let probe = bare_probe();
        let rungs = ladder(&PerModelAdapter, &probe, Some("off-catalog")).await;
        assert_eq!(reconcile(Some("ultra"), &rungs), None);
    }
}
