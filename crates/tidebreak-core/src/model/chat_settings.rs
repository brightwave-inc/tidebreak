use serde::{Deserialize, Serialize};

/// Who authored a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The system prompt / instructions.
    System,
    /// Input from the human user.
    User,
    /// Output from the model.
    Assistant,
    /// A tool result fed back into the model.
    Tool,
}

/// How hard a reasoning-capable model should think before answering.
///
/// The scale runs from [`Self::None`] to [`Self::Max`] and the ordering is the
/// scale itself, not an implementation detail: comparisons and
/// [`Self::clamp_to`] rely on it.
///
/// No model accepts the whole scale. `none` is an OpenAI level that the Claude
/// family rejects, `max` is missing from several rows on both routes, and some
/// models take no effort control at all. A model's accepted levels live on its
/// registry entry; a stored choice is mapped onto them with [`Self::clamp_to`]
/// before a request is built.
///
/// Persisted per chat as the token from [`Self::as_str`] and threaded into the
/// model request for each turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    /// Answer without spending reasoning tokens at all.
    ///
    /// Distinct from an absent override, which leaves the provider's own
    /// default in force.
    None,
    /// Minimize reasoning tokens for the fastest, cheapest response.
    Low,
    /// The provider's balanced default.
    Medium,
    /// Spend more reasoning tokens for harder problems.
    High,
    /// Above `High`: the level recommended for coding and agentic work.
    #[serde(rename = "xhigh")]
    XHigh,
    /// The most reasoning a model will do, at the highest latency and cost.
    Max,
    /// The top of the ladder, above `Max`.
    ///
    /// Not a plain "even more tokens" step. Engines that offer it spend the
    /// level on something structural: Codex advertises `ultra` per model, and
    /// Claude Code's own top rung is ultracode, which pairs `xhigh` with
    /// multi-agent orchestration. Chat models do not accept it, so
    /// [`Self::clamp_to`] degrades it to whatever the row does take.
    Ultra,
}

impl ReasoningEffort {
    /// Every level, in ascending order.
    pub const ALL: &'static [Self] = &[
        Self::None,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::XHigh,
        Self::Max,
        Self::Ultra,
    ];

    /// The wire/storage token for this effort level.
    ///
    /// Providers spell the level above `high` as one word, so this is not the
    /// `snake_case` rendering of the variant name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
            Self::Ultra => "ultra",
        }
    }

    /// Parse a stored/wire token back into an effort level.
    ///
    /// Deliberately returns `Option` (invalid tokens are dropped, not errored),
    /// so this is not the `FromStr` trait.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|level| level.as_str() == value)
    }

    /// The level to actually send to a model that accepts only `supported`.
    ///
    /// A chat's effort outlives the model it was chosen for: switching the chat
    /// to a narrower model, or a provider retiring a level, both leave a stored
    /// choice the model would reject. Rather than fail a turn on a hint, the
    /// request degrades to the closest level the model does take — the highest
    /// at or below the request, or the model's lowest when the request sits
    /// under its whole range. That matches the degradation Anthropic documents
    /// for `xhigh` on models without it.
    ///
    /// A model that accepts no levels yields `None`, and the parameter is left
    /// off the request entirely.
    #[must_use]
    pub fn clamp_to(self, supported: &[Self]) -> Option<Self> {
        supported
            .iter()
            .copied()
            .filter(|level| *level <= self)
            .max()
            .or_else(|| supported.iter().copied().min())
    }
}

/// Network access granted to commands in one conversation workspace.
///
/// The policy is provider-neutral. Providers compile it to their strongest
/// available enforcement mechanism; the local native adapter exposes only one
/// loopback broker port and applies the destination decision outside the
/// sandbox. Open access still excludes local, private, and link-local targets.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum NetworkPolicy {
    /// Deny every outbound connection.
    Off,
    /// Permit only the fixed package-registry destination class.
    PackageManagers,
    /// Permit an explicit host list and, optionally, the package-registry
    /// destination class. Hosts are exact DNS names, never wildcard patterns.
    AllowedHosts {
        allowed_hosts: Vec<String>,
        #[serde(default)]
        package_managers: bool,
    },
    /// Permit public-internet destinations. Local, private, and link-local
    /// addresses remain unreachable through the local broker.
    #[default]
    Open,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_effort_tokens_round_trip_and_keep_the_original_three() {
        for level in ReasoningEffort::ALL {
            assert_eq!(ReasoningEffort::from_str(level.as_str()), Some(*level));
            assert_eq!(
                serde_json::to_string(level).unwrap(),
                format!("\"{}\"", level.as_str())
            );
            assert_eq!(
                serde_json::from_str::<ReasoningEffort>(&format!("\"{}\"", level.as_str()))
                    .unwrap(),
                *level
            );
        }
        // The three levels that shipped before the scale widened are stored in
        // chat rows and must keep parsing to the same variants.
        assert_eq!(ReasoningEffort::from_str("low"), Some(ReasoningEffort::Low));
        assert_eq!(
            ReasoningEffort::from_str("medium"),
            Some(ReasoningEffort::Medium)
        );
        assert_eq!(
            ReasoningEffort::from_str("high"),
            Some(ReasoningEffort::High)
        );
        // The level above `high` is one word on the wire, not the snake_case
        // rendering of its variant name.
        assert_eq!(ReasoningEffort::XHigh.as_str(), "xhigh");
        assert!(ReasoningEffort::from_str("x_high").is_none());
        assert!(ReasoningEffort::from_str("").is_none());
        assert!(ReasoningEffort::from_str("HIGH").is_none());
    }

    #[test]
    fn reasoning_effort_orders_from_none_to_ultra() {
        assert!(ReasoningEffort::ALL
            .windows(2)
            .all(|pair| pair[0] < pair[1]));
        assert_eq!(ReasoningEffort::ALL.first(), Some(&ReasoningEffort::None));
        assert_eq!(ReasoningEffort::ALL.last(), Some(&ReasoningEffort::Ultra));
    }

    /// `Ultra` is above `Max`, and no chat model accepts it: every catalog row
    /// stops at `max` or lower. A session that carried the level over from an
    /// engine that does offer it degrades to the row's own top rather than
    /// failing the turn on a hint.
    #[test]
    fn ultra_degrades_for_a_model_whose_ladder_stops_lower() {
        assert!(ReasoningEffort::Ultra > ReasoningEffort::Max);
        assert_eq!(
            ReasoningEffort::Ultra.clamp_to(&[
                ReasoningEffort::Low,
                ReasoningEffort::High,
                ReasoningEffort::Max,
            ]),
            Some(ReasoningEffort::Max)
        );
        assert_eq!(
            ReasoningEffort::Ultra.clamp_to(&[ReasoningEffort::Low, ReasoningEffort::XHigh]),
            Some(ReasoningEffort::XHigh)
        );
        assert_eq!(
            ReasoningEffort::Ultra.clamp_to(&[ReasoningEffort::Ultra]),
            Some(ReasoningEffort::Ultra)
        );
        assert_eq!(ReasoningEffort::Ultra.clamp_to(&[]), None);
        assert_eq!(
            ReasoningEffort::from_str("ultra"),
            Some(ReasoningEffort::Ultra)
        );
    }

    #[test]
    fn an_unsupported_effort_degrades_to_the_closest_level_the_model_takes() {
        let anthropic = &[
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
            ReasoningEffort::Max,
        ];
        let no_max = &[
            ReasoningEffort::None,
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
        ];
        let classic = &[
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
        ];

        // A supported level passes through untouched.
        for level in anthropic {
            assert_eq!(level.clamp_to(anthropic), Some(*level));
        }
        // Above the range: down to the highest level on offer.
        assert_eq!(
            ReasoningEffort::Max.clamp_to(no_max),
            Some(ReasoningEffort::XHigh)
        );
        assert_eq!(
            ReasoningEffort::XHigh.clamp_to(classic),
            Some(ReasoningEffort::High)
        );
        // Below the range: up to the lowest level on offer, because a model
        // that reasons at all cannot be told to stop.
        assert_eq!(
            ReasoningEffort::None.clamp_to(anthropic),
            Some(ReasoningEffort::Low)
        );
        // No control at all: the parameter is dropped.
        assert_eq!(ReasoningEffort::High.clamp_to(&[]), None);
    }
}
