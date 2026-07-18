//! Provider-neutral model reranking.

use async_trait::async_trait;

use crate::{Result, RetrievalError, ScoredChunk};

/// Assigns query-specific relevance scores to an existing candidate set.
///
/// Implementations must return exactly one finite score for every input chunk,
/// in the same order as `candidates`. The retrieval pipeline validates that
/// contract before changing any candidate score or rank. Scores are
/// provider-specific and must not be compared across queries, models, or
/// reranker configurations.
#[async_trait]
pub trait Reranker: Send + Sync {
    /// Whether reranking is guaranteed to stay in-process without network or
    /// external-service access.
    ///
    /// Defaults to `false` so new provider-backed implementations fail closed at
    /// approval boundaries until they explicitly prove they are local.
    fn is_local(&self) -> bool {
        false
    }

    /// Score every candidate for `query`, preserving input alignment.
    /// Implementations must use [`crate::Chunk::retrieval_text`] as the
    /// candidate text so structural context matches embedding and lexical inputs.
    async fn rerank(&self, query: &str, candidates: &[ScoredChunk]) -> Result<Vec<f32>>;
}

/// Apply an optional reranker and stably order candidates by its scores.
///
/// Empty candidate sets bypass the provider. Validation happens before mutation
/// so malformed provider output never leaves a partially reranked result set.
pub(crate) async fn rerank_candidates(
    reranker: Option<&dyn Reranker>,
    query: &str,
    mut candidates: Vec<ScoredChunk>,
) -> Result<Vec<ScoredChunk>> {
    let Some(reranker) = reranker else {
        return Ok(candidates);
    };
    if candidates.is_empty() {
        return Ok(candidates);
    }

    let scores = reranker
        .rerank(query, &candidates)
        .await
        .map_err(|error| match error {
            error @ RetrievalError::Rerank(_) => error,
            error => RetrievalError::rerank(error.to_string()),
        })?;
    if scores.len() != candidates.len() {
        return Err(RetrievalError::rerank(format!(
            "reranker returned {} scores for {} candidates",
            scores.len(),
            candidates.len()
        )));
    }
    if let Some((index, score)) = scores
        .iter()
        .copied()
        .enumerate()
        .find(|(_, score)| !score.is_finite())
    {
        return Err(RetrievalError::rerank(format!(
            "reranker returned non-finite score {score} at index {index}"
        )));
    }

    for (candidate, score) in candidates.iter_mut().zip(scores) {
        candidate.score = score;
    }
    // Stable sorting preserves backend order for exact reranker ties.
    candidates.sort_by(|left, right| right.score.total_cmp(&left.score));
    Ok(candidates)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::{ByteSpan, Chunk, DocumentId};

    struct FixedReranker {
        scores: Vec<f32>,
        calls: AtomicUsize,
    }

    struct RerankFailure;
    struct EmbedFailure;

    #[async_trait]
    impl Reranker for FixedReranker {
        async fn rerank(&self, _query: &str, _candidates: &[ScoredChunk]) -> Result<Vec<f32>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.scores.clone())
        }
    }

    #[async_trait]
    impl Reranker for RerankFailure {
        async fn rerank(&self, _query: &str, _candidates: &[ScoredChunk]) -> Result<Vec<f32>> {
            Err(RetrievalError::rerank("provider unavailable"))
        }
    }

    #[async_trait]
    impl Reranker for EmbedFailure {
        async fn rerank(&self, _query: &str, _candidates: &[ScoredChunk]) -> Result<Vec<f32>> {
            Err(RetrievalError::embed("provider returned secret details"))
        }
    }

    fn candidates(count: usize) -> Vec<ScoredChunk> {
        let document_id = DocumentId::new();
        (0..count)
            .map(|ordinal| ScoredChunk {
                chunk: Chunk::new(
                    document_id,
                    ordinal,
                    ByteSpan::new(ordinal * 10, ordinal * 10 + 5),
                    ordinal.to_string(),
                ),
                source: crate::DocumentSource::Inline,
                generation: None,
                score: 10.0 - ordinal as f32,
            })
            .collect()
    }

    #[tokio::test]
    async fn replaces_scores_reorders_and_preserves_ties() {
        let reranker = FixedReranker {
            scores: vec![0.2, 0.9, 0.9, 0.1],
            calls: AtomicUsize::new(0),
        };
        let hits = rerank_candidates(Some(&reranker), "query", candidates(4))
            .await
            .unwrap();
        assert_eq!(
            hits.iter().map(|hit| hit.chunk.ordinal).collect::<Vec<_>>(),
            vec![1, 2, 0, 3]
        );
        assert_eq!(
            hits.iter().map(|hit| hit.score).collect::<Vec<_>>(),
            vec![0.9, 0.9, 0.2, 0.1]
        );
    }

    #[tokio::test]
    async fn bypasses_empty_but_reranks_a_singleton() {
        let reranker = FixedReranker {
            scores: vec![0.25],
            calls: AtomicUsize::new(0),
        };
        assert!(rerank_candidates(Some(&reranker), "query", vec![])
            .await
            .unwrap()
            .is_empty());
        let singleton = rerank_candidates(Some(&reranker), "query", candidates(1))
            .await
            .unwrap();
        assert_eq!(singleton[0].score, 0.25);
        assert_eq!(reranker.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn rejects_bad_output_and_propagates_provider_failure() {
        for scores in [vec![0.1], vec![0.1, f32::NAN], vec![0.1, f32::INFINITY]] {
            let reranker = FixedReranker {
                scores,
                calls: AtomicUsize::new(0),
            };
            let error = rerank_candidates(Some(&reranker), "query", candidates(2))
                .await
                .unwrap_err();
            assert!(matches!(error, RetrievalError::Rerank(_)));
        }

        let error = rerank_candidates(Some(&RerankFailure), "query", candidates(2))
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "reranking error: provider unavailable");

        let error = rerank_candidates(Some(&EmbedFailure), "query", candidates(2))
            .await
            .unwrap_err();
        assert!(matches!(error, RetrievalError::Rerank(_)));
        assert_eq!(
            error.to_string(),
            "reranking error: embedding error: provider returned secret details"
        );
    }

    #[test]
    fn is_object_safe() {
        fn accepts_arc(_: std::sync::Arc<dyn Reranker>) {}
        accepts_arc(std::sync::Arc::new(FixedReranker {
            scores: vec![],
            calls: AtomicUsize::new(0),
        }));
    }
}
