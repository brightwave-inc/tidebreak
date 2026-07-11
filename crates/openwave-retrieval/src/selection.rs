//! Post-retrieval result selection shared by the HTTP and tool entry points.
//!
//! Vector backends remain responsible only for corpus filtering and relevance
//! ranking. Their native choice among exact ties at the candidate boundary is
//! backend-defined; this module applies deterministic policy to the ranked
//! candidates it receives.

use std::collections::HashSet;

use crate::document::{ByteSpan, ScoredChunk};
use crate::id::DocumentId;

/// Maximum number of passages any public search surface returns.
pub const MAX_SEARCH_RESULTS: usize = 50;

const CANDIDATE_MULTIPLIER: usize = 4;
const MAX_SEARCH_CANDIDATES: usize = MAX_SEARCH_RESULTS * CANDIDATE_MULTIPLIER;
const DIVERSITY_LOOKAHEAD: usize = 3;

/// Clamp a low-level retrieval request without changing `k == 0` semantics.
pub(crate) fn result_limit(k: usize) -> usize {
    k.min(MAX_SEARCH_RESULTS)
}

/// Candidate count requested from the backend to allow policy backfill.
pub(crate) fn candidate_limit(k: usize) -> usize {
    k.saturating_mul(CANDIDATE_MULTIPLIER)
        .min(MAX_SEARCH_CANDIDATES)
}

/// Select up to `k` candidates while suppressing redundant same-document spans
/// and gently preferring document diversity within a four-result rank window.
///
/// Diversity can skip over the baseline candidate, but skipped candidates stay
/// available for later rounds. The final results are restored to input rank
/// order (backend rank, or reranker rank when configured), so selection changes
/// membership without inventing a new score order.
pub(crate) fn select(candidates: Vec<ScoredChunk>, k: usize) -> Vec<ScoredChunk> {
    if k == 0 || candidates.is_empty() {
        return Vec::new();
    }

    let mut remaining: Vec<usize> = (0..candidates.len()).collect();
    let mut selected = Vec::with_capacity(k.min(candidates.len()));
    let mut seen_documents = HashSet::<DocumentId>::new();

    while selected.len() < k {
        remaining.retain(|&index| {
            !selected.iter().any(|&selected_index| {
                substantially_overlaps(&candidates[index], &candidates[selected_index])
            })
        });
        if remaining.is_empty() {
            break;
        }

        let window_end = remaining
            .len()
            .min(1usize.saturating_add(DIVERSITY_LOOKAHEAD));
        let chosen_position = remaining[..window_end]
            .iter()
            .position(|&index| !seen_documents.contains(&candidates[index].chunk.document_id))
            .unwrap_or(0);
        let chosen_index = remaining.remove(chosen_position);
        seen_documents.insert(candidates[chosen_index].chunk.document_id);
        selected.push(chosen_index);
    }

    selected.sort_unstable();
    selected
        .into_iter()
        .map(|index| candidates[index].clone())
        .collect()
}

fn substantially_overlaps(left: &ScoredChunk, right: &ScoredChunk) -> bool {
    left.chunk.document_id == right.chunk.document_id
        && overlap_exceeds_three_tenths(left.chunk.span, right.chunk.span)
}

fn overlap_exceeds_three_tenths(left: ByteSpan, right: ByteSpan) -> bool {
    let shorter = left.len().min(right.len());
    if shorter == 0 {
        return false;
    }
    let intersection = left
        .end
        .min(right.end)
        .saturating_sub(left.start.max(right.start));
    (intersection as u128) * 10 > (shorter as u128) * 3
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Chunk;
    use crate::id::DocumentId;

    fn scored(document_id: DocumentId, ordinal: usize, start: usize, end: usize) -> ScoredChunk {
        ScoredChunk {
            chunk: Chunk::new(
                document_id,
                ordinal,
                ByteSpan::new(start, end),
                format!("chunk {ordinal}"),
            ),
            score: 1.0 - ordinal as f32 / 100.0,
        }
    }

    #[test]
    fn candidate_limit_is_four_times_output_and_capped() {
        assert_eq!(result_limit(0), 0);
        assert_eq!(result_limit(1), 1);
        assert_eq!(result_limit(usize::MAX), MAX_SEARCH_RESULTS);
        assert_eq!(candidate_limit(0), 0);
        assert_eq!(candidate_limit(1), 4);
        assert_eq!(candidate_limit(17), 68);
        assert_eq!(candidate_limit(MAX_SEARCH_RESULTS), 200);
        assert_eq!(candidate_limit(usize::MAX), 200);
    }

    #[test]
    fn overlap_uses_strict_three_tenths_of_the_shorter_span() {
        assert!(!overlap_exceeds_three_tenths(
            ByteSpan::new(0, 10),
            ByteSpan::new(7, 17)
        ));
        assert!(overlap_exceeds_three_tenths(
            ByteSpan::new(0, 10),
            ByteSpan::new(6, 16)
        ));
        assert!(!overlap_exceeds_three_tenths(
            ByteSpan::new(0, 0),
            ByteSpan::new(0, 10)
        ));
        assert!(!overlap_exceeds_three_tenths(
            ByteSpan::new(0, 10),
            ByteSpan::new(10, 20)
        ));
    }

    #[test]
    fn overlap_suppression_is_limited_to_the_same_document() {
        let first = DocumentId::new();
        let second = DocumentId::new();
        let selected = select(vec![scored(first, 0, 0, 10), scored(second, 1, 0, 10)], 2);
        assert_eq!(selected.len(), 2);
    }

    #[test]
    fn a_contained_same_document_span_is_suppressed() {
        let document = DocumentId::new();
        let selected = select(
            vec![scored(document, 0, 0, 100), scored(document, 1, 30, 60)],
            2,
        );
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].chunk.ordinal, 0);
    }

    #[test]
    fn diversity_prefers_first_unseen_document_in_rank_window() {
        let first = DocumentId::new();
        let second = DocumentId::new();
        let candidates = vec![
            scored(first, 0, 0, 10),
            scored(first, 1, 20, 30),
            scored(first, 2, 40, 50),
            scored(second, 3, 0, 10),
        ];
        let selected = select(candidates, 2);
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].chunk.document_id, first);
        assert_eq!(selected[1].chunk.document_id, second);
    }

    #[test]
    fn diversity_does_not_reach_beyond_three_lookahead_candidates() {
        let first = DocumentId::new();
        let second = DocumentId::new();
        let candidates = vec![
            scored(first, 0, 0, 10),
            scored(first, 1, 20, 30),
            scored(first, 2, 40, 50),
            scored(first, 3, 60, 70),
            scored(first, 4, 80, 90),
            scored(second, 5, 0, 10),
        ];
        let selected = select(candidates, 2);
        assert_eq!(selected[1].chunk.ordinal, 1);
    }

    #[test]
    fn skipped_candidates_remain_and_membership_returns_in_original_order() {
        let first = DocumentId::new();
        let second = DocumentId::new();
        let candidates = vec![
            scored(first, 0, 0, 10),
            scored(first, 1, 20, 30),
            scored(second, 2, 0, 10),
        ];
        let selected = select(candidates, 3);
        let ordinals: Vec<_> = selected
            .into_iter()
            .map(|candidate| candidate.chunk.ordinal)
            .collect();
        assert_eq!(ordinals, vec![0, 1, 2]);
    }

    #[test]
    fn overlapping_candidates_are_suppressed_and_later_candidates_backfill() {
        let first = DocumentId::new();
        let second = DocumentId::new();
        let candidates = vec![
            scored(first, 0, 0, 100),
            scored(first, 1, 20, 80),
            scored(second, 2, 0, 10),
        ];
        let selected = select(candidates, 2);
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].chunk.ordinal, 0);
        assert_eq!(selected[1].chunk.ordinal, 2);
    }

    #[test]
    fn scores_do_not_affect_selected_membership() {
        let first = DocumentId::new();
        let second = DocumentId::new();
        let candidates = vec![
            scored(first, 0, 0, 100),
            scored(first, 1, 20, 80),
            scored(second, 2, 0, 20),
        ];
        let mut rescored = candidates.clone();
        rescored[0].score = -500.0;
        rescored[1].score = 10_000.0;
        rescored[2].score = f32::NAN;

        let original_ids: Vec<_> = select(candidates, 2)
            .into_iter()
            .map(|candidate| candidate.chunk.id)
            .collect();
        let rescored_ids: Vec<_> = select(rescored, 2)
            .into_iter()
            .map(|candidate| candidate.chunk.id)
            .collect();
        assert_eq!(original_ids, rescored_ids);
    }
}
