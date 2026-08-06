//! Infrequent hard chat compaction — trigger math and boundary selection.
//!
//! Compaction runs when an unabridged transcript estimate exceeds a
//! context-scaled threshold, then cuts the raw prefix hard toward a much
//! smaller target so the next trigger is far away (hysteresis). Deterministic
//! [`crate::context::fit_to_budget`] remains the always-on safety net; this
//! module only decides *when* and *how far* a semantic checkpoint may advance.

use crate::context;
use crate::id::MessageId;
use crate::model::Role;
use crate::provider::ChatMessage;

/// Default fraction of the model context window that triggers compaction.
pub const DEFAULT_COMPACTION_THRESHOLD_FRACTION: f64 = 0.75;

/// Default fraction of the model context window left as raw history after a
/// hard compaction.
pub const DEFAULT_COMPACTION_TARGET_FRACTION: f64 = 0.25;

/// Floor on the trigger threshold so short-context models do not compact at
/// tiny absolute sizes.
pub const DEFAULT_COMPACTION_MIN_THRESHOLD_TOKENS: usize = 50_000;

/// Durable transcript rows kept raw at the tail of every compaction.
pub const DEFAULT_COMPACTION_PROTECT_RECENT_MESSAGES: usize = 5;

/// Host-tunable compaction cadence and retention.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionPolicy {
    /// Compact when unabridged tokens exceed this fraction of `context_window`.
    pub threshold_fraction: f64,
    /// After compaction, keep about this fraction of `context_window` as raw
    /// recent history (plus the checkpoint).
    pub target_fraction: f64,
    /// Absolute floor applied before scaling by context window.
    pub min_threshold_tokens: usize,
    /// Newest durable messages that must never enter the compacted prefix.
    pub protect_recent_messages: usize,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            threshold_fraction: DEFAULT_COMPACTION_THRESHOLD_FRACTION,
            target_fraction: DEFAULT_COMPACTION_TARGET_FRACTION,
            min_threshold_tokens: DEFAULT_COMPACTION_MIN_THRESHOLD_TOKENS,
            protect_recent_messages: DEFAULT_COMPACTION_PROTECT_RECENT_MESSAGES,
        }
    }
}

impl CompactionPolicy {
    /// Resolve absolute token threshold and target for one model window.
    ///
    /// Mirrors the production hysteresis rule: scale fractions by the window,
    /// floor the threshold by `min_threshold_tokens` (capped at the window),
    /// and when the floor raises the threshold, raise the target by the same
    /// threshold/target ratio so the gap stays "infrequent and hard".
    pub fn resolve_token_bounds(&self, context_window: usize) -> CompactionTokenBounds {
        let context_window = context_window.max(1);
        let scaled_threshold =
            ((context_window as f64) * self.threshold_fraction.clamp(0.0, 1.0)).floor() as usize;
        let scaled_target =
            ((context_window as f64) * self.target_fraction.clamp(0.0, 1.0)).floor() as usize;
        let mut threshold = context_window.min(self.min_threshold_tokens.max(scaled_threshold));
        let mut target = scaled_target.min(threshold.saturating_sub(1));
        if threshold > scaled_threshold && self.threshold_fraction > 0.0 {
            let ratio = self.target_fraction / self.threshold_fraction;
            target = target.max(((threshold as f64) * ratio).floor() as usize);
            target = target.min(threshold.saturating_sub(1));
        }
        if threshold == 0 {
            threshold = 1;
        }
        CompactionTokenBounds { threshold, target }
    }
}

/// Absolute trigger and post-compaction raw-history budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionTokenBounds {
    /// Compact when unabridged tokens are strictly greater than this.
    pub threshold: usize,
    /// Keep at most this many raw tokens after the checkpoint boundary.
    pub target: usize,
}

/// What the compaction trigger counts, measured once per transcript load.
///
/// Two numbers rather than one because both would otherwise lie. Tool results
/// are byte-capped for the provider body, so counting the live transcript
/// alone under-counts history and compaction fires late; counting only the
/// uncapped rebuild freezes at load, so a turn that crosses the threshold on
/// its own tool output never compacts. The trigger is therefore the uncapped
/// history plus whatever this turn has appended to the transcript since.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionTokenBaseline {
    /// Loaded durable history, rebuilt without tool-result caps.
    pub unabridged_history_tokens: usize,
    /// The same history as the live provider transcript started out.
    pub loaded_transcript_tokens: usize,
}

impl CompactionTokenBaseline {
    /// Trigger tokens for the transcript as it stands now.
    #[must_use]
    pub fn trigger_tokens(&self, transcript: &[ChatMessage]) -> usize {
        let live = context::estimate_transcript_tokens(transcript);
        self.unabridged_history_tokens
            .saturating_add(live.saturating_sub(self.loaded_transcript_tokens))
    }
}

/// One durable row's inclusive end in the rebuilt provider transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionSourceBoundary {
    pub message_id: MessageId,
    pub role: Role,
    /// Exclusive end index in the provider transcript (`transcript[..boundary]`
    /// is covered when this row is the inclusive checkpoint source).
    pub provider_boundary: usize,
}

/// Inclusive durable boundary chosen for the next checkpoint, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionSelection {
    pub message_id: MessageId,
    pub provider_boundary: usize,
}

/// Decide whether the transcript needs hard compaction and, if so, which
/// durable message is the inclusive end of the compacted prefix.
///
/// Selection walks durable message ids (and their rebuilt provider spans),
/// never a raw count of provider messages — tool expansion must not shift the
/// boundary. The newest `protect_recent_messages` durable rows stay raw. When
/// that protected tail alone exceeds `token_target`, or nothing remains to
/// compact, this returns `None` (fail open to deterministic fitting).
pub fn select_compaction_boundary(
    transcript: &[ChatMessage],
    source_boundaries: &[CompactionSourceBoundary],
    token_target: usize,
    protect_recent_messages: usize,
    current_provider_boundary: Option<usize>,
) -> Option<CompactionSelection> {
    let n = source_boundaries.len();
    if n == 0 || transcript.is_empty() {
        return None;
    }
    let protect = protect_recent_messages.min(n);
    if protect == n {
        return None;
    }
    let must_keep_start = n - protect;
    let keep_from_provider = provider_span_start(source_boundaries, must_keep_start);
    let must_keep_tokens = context::estimate_transcript_tokens(&transcript[keep_from_provider..]);
    if must_keep_tokens > token_target {
        return None;
    }

    // Keep additional newest unprotected messages while they fit the target.
    let mut keep_from_idx = must_keep_start;
    let mut budget = token_target - must_keep_tokens;
    for i in (0..must_keep_start).rev() {
        let start = provider_span_start(source_boundaries, i);
        let end = source_boundaries[i].provider_boundary.min(transcript.len());
        if start >= end {
            continue;
        }
        let cost = context::estimate_transcript_tokens(&transcript[start..end]);
        if cost > budget {
            break;
        }
        budget -= cost;
        keep_from_idx = i;
    }

    if keep_from_idx == 0 {
        return None;
    }
    let candidate = source_boundaries[keep_from_idx - 1];
    if candidate.provider_boundary == 0 || candidate.provider_boundary > transcript.len() {
        return None;
    }
    if current_provider_boundary.is_some_and(|boundary| candidate.provider_boundary <= boundary) {
        return None;
    }
    Some(CompactionSelection {
        message_id: candidate.message_id,
        provider_boundary: candidate.provider_boundary,
    })
}

fn provider_span_start(source_boundaries: &[CompactionSourceBoundary], index: usize) -> usize {
    if index == 0 {
        0
    } else {
        source_boundaries[index - 1].provider_boundary
    }
}

/// Collect user-visible asks from the compacted durable prefix for
/// `original_requests` carry-forward.
pub fn user_asks_in_prefix(
    source_boundaries: &[CompactionSourceBoundary],
    provider_boundary: usize,
    user_texts: &[(MessageId, String)],
) -> Vec<String> {
    let covered: std::collections::HashSet<MessageId> = source_boundaries
        .iter()
        .filter(|source| source.provider_boundary <= provider_boundary)
        .filter(|source| source.role == Role::User)
        .map(|source| source.message_id)
        .collect();
    user_texts
        .iter()
        .filter(|(id, _)| covered.contains(id))
        .map(|(_, text)| text.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::MessageId;
    use crate::provider::ChatMessage;

    fn boundary(
        message_id: MessageId,
        role: Role,
        provider_boundary: usize,
    ) -> CompactionSourceBoundary {
        CompactionSourceBoundary {
            message_id,
            role,
            provider_boundary,
        }
    }

    #[test]
    fn resolve_token_bounds_scales_and_floors_small_windows() {
        let policy = CompactionPolicy::default();
        let large = policy.resolve_token_bounds(200_000);
        assert_eq!(large.threshold, 150_000);
        assert_eq!(large.target, 50_000);

        let small = policy.resolve_token_bounds(3_000);
        assert_eq!(small.threshold, 3_000);
        assert!(small.target < small.threshold);
        assert!(small.target >= 750);
    }

    #[test]
    fn select_boundary_protects_recent_tail_and_advances() {
        let ids: Vec<MessageId> = (0..6).map(|_| MessageId::new()).collect();
        let transcript: Vec<ChatMessage> = (0..6)
            .map(|i| ChatMessage::text(Role::User, format!("msg-{i} {}", "x".repeat(300))))
            .collect();
        // Each message is one provider row.
        let sources: Vec<_> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| boundary(*id, Role::User, i + 1))
            .collect();
        let tokens = context::estimate_transcript_tokens(&transcript);
        // Room for the two protected rows plus one more, so the boundary has
        // somewhere to advance to. A target below the protected tail is the
        // separate fail-open case.
        let target = tokens / 2;
        let selected =
            select_compaction_boundary(&transcript, &sources, target, 2, None).expect("boundary");
        assert!(selected.provider_boundary < transcript.len());
        assert!(selected.provider_boundary <= 4);
        let remaining =
            context::estimate_transcript_tokens(&transcript[selected.provider_boundary..]);
        assert!(remaining <= target);

        assert!(
            select_compaction_boundary(
                &transcript,
                &sources,
                target,
                2,
                Some(selected.provider_boundary),
            )
            .is_none(),
            "must not rewrite an equal or newer boundary"
        );
    }

    #[test]
    fn select_boundary_fails_open_when_protect_tail_exceeds_target() {
        let ids: Vec<MessageId> = (0..3).map(|_| MessageId::new()).collect();
        let transcript: Vec<ChatMessage> = ids
            .iter()
            .map(|_| ChatMessage::text(Role::User, "huge ".repeat(2_000)))
            .collect();
        let sources: Vec<_> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| boundary(*id, Role::User, i + 1))
            .collect();
        assert!(select_compaction_boundary(&transcript, &sources, 10, 2, None).is_none());
    }

    #[test]
    fn trigger_tokens_count_history_uncapped_and_in_turn_growth() {
        let loaded = vec![ChatMessage::text(Role::User, "abridged history")];
        let baseline = CompactionTokenBaseline {
            // As if the durable tool results were far larger uncapped.
            unabridged_history_tokens: 10_000,
            loaded_transcript_tokens: context::estimate_transcript_tokens(&loaded),
        };
        assert_eq!(baseline.trigger_tokens(&loaded), 10_000);

        let mut grown = loaded.clone();
        grown.push(ChatMessage::text(
            Role::Assistant,
            "tool output ".repeat(500),
        ));
        let growth = context::estimate_transcript_tokens(&grown[1..]);
        assert!(growth > 0);
        assert_eq!(baseline.trigger_tokens(&grown), 10_000 + growth);
    }

    #[test]
    fn select_boundary_fails_open_when_everything_is_protected() {
        let id = MessageId::new();
        let transcript = vec![ChatMessage::text(Role::User, "only")];
        let sources = vec![boundary(id, Role::User, 1)];
        assert!(select_compaction_boundary(&transcript, &sources, 1, 5, None).is_none());
    }
}
