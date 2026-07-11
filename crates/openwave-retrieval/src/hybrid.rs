//! Dependency-free lexical ranking and reciprocal rank fusion.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::{ChunkId, Embedding, ScoredChunk, VectorRecord};

const BM25_K1: f32 = 1.2;
const BM25_B: f32 = 0.75;
const RRF_K: f32 = 60.0;

pub(crate) fn rank(
    records: &[&VectorRecord],
    query_text: &str,
    query_embedding: &Embedding,
    k: usize,
    min_dense_similarity: f32,
) -> Vec<ScoredChunk> {
    let dense = dense(records, query_embedding, k, min_dense_similarity);
    let lexical = bm25(records, query_text, k);
    let mut fused = HashMap::new();
    for (position, hit) in dense.into_iter().enumerate() {
        add_rank(&mut fused, &hit.chunk, position);
    }
    for (position, (record, _)) in lexical.into_iter().enumerate() {
        add_rank(&mut fused, &record.chunk, position);
    }

    let mut hits = fused
        .into_values()
        .map(|(chunk, score)| ScoredChunk { chunk, score })
        .collect::<Vec<_>>();
    sort_hits(&mut hits);
    hits.truncate(k);
    hits
}

pub(crate) fn dense(
    records: &[&VectorRecord],
    query_embedding: &Embedding,
    k: usize,
    min_similarity: f32,
) -> Vec<ScoredChunk> {
    let mut hits = records
        .iter()
        .map(|record| ScoredChunk {
            chunk: record.chunk.clone(),
            score: query_embedding.cosine_similarity(&record.embedding),
        })
        .filter(|hit| hit.score >= min_similarity)
        .collect::<Vec<_>>();
    sort_hits(&mut hits);
    hits.truncate(k);
    hits
}

fn add_rank(
    fused: &mut HashMap<ChunkId, (crate::Chunk, f32)>,
    chunk: &crate::Chunk,
    position: usize,
) {
    let entry = fused
        .entry(chunk.id)
        .or_insert_with(|| (chunk.clone(), 0.0));
    entry.1 += 1.0 / (RRF_K + position as f32 + 1.0);
}

fn bm25<'a>(records: &[&'a VectorRecord], query: &str, k: usize) -> Vec<(&'a VectorRecord, f32)> {
    let query_terms = tokenize(query).into_iter().collect::<BTreeSet<_>>();
    if query_terms.is_empty() || records.is_empty() {
        return Vec::new();
    }

    let documents = records
        .iter()
        .map(|record| tokenize(&record.chunk.retrieval_text()))
        .collect::<Vec<_>>();
    let average_length =
        documents.iter().map(Vec::len).sum::<usize>() as f32 / documents.len() as f32;
    let mut document_frequency = BTreeMap::new();
    for terms in &documents {
        let matching = terms
            .iter()
            .filter(|term| query_terms.contains(*term))
            .collect::<BTreeSet<_>>();
        for term in matching {
            *document_frequency.entry(term.clone()).or_insert(0usize) += 1;
        }
    }

    let corpus_size = records.len() as f32;
    let mut scored = records
        .iter()
        .zip(documents)
        .filter_map(|(record, terms)| {
            let length = terms.len() as f32;
            let mut frequencies = BTreeMap::new();
            for term in terms {
                if query_terms.contains(&term) {
                    *frequencies.entry(term).or_insert(0usize) += 1;
                }
            }
            let score = frequencies
                .into_iter()
                .fold(0.0, |score, (term, frequency)| {
                    let frequency = frequency as f32;
                    let document_frequency = *document_frequency.get(&term).unwrap_or(&0) as f32;
                    let inverse_document_frequency = (1.0
                        + (corpus_size - document_frequency + 0.5) / (document_frequency + 0.5))
                        .ln();
                    let normalized_length = if average_length == 0.0 {
                        1.0
                    } else {
                        length / average_length
                    };
                    score
                        + inverse_document_frequency * frequency * (BM25_K1 + 1.0)
                            / (frequency + BM25_K1 * (1.0 - BM25_B + BM25_B * normalized_length))
                });
            (score > 0.0).then_some((*record, score))
        })
        .collect::<Vec<_>>();
    sort_ranked(&mut scored);
    scored.truncate(k);
    scored
}

fn sort_ranked(ranked: &mut [(&VectorRecord, f32)]) {
    ranked.sort_by(|(left_record, left_score), (right_record, right_score)| {
        right_score
            .total_cmp(left_score)
            .then_with(|| left_record.chunk.id.0.cmp(&right_record.chunk.id.0))
    });
}

fn sort_hits(hits: &mut [ScoredChunk]) {
    hits.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.chunk.id.0.cmp(&right.chunk.id.0))
    });
}

fn tokenize(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ByteSpan, Chunk, DocumentId};

    fn record(text: &str, vector: Vec<f32>) -> VectorRecord {
        let document_id = DocumentId::new();
        VectorRecord {
            project_id: None,
            chunk: Chunk::new(document_id, 0, ByteSpan::new(0, text.len()), text),
            embedding: Embedding(vector),
        }
    }

    #[test]
    fn lexical_and_dense_candidates_are_both_fused() {
        let lexical = record("ZXQ-4412 repair bulletin", vec![0.0, 1.0]);
        let dense = record("ordinary semantic match", vec![1.0, 0.0]);
        let records = [&lexical, &dense];

        let hits = rank(
            &records,
            "zxq 4412",
            &Embedding(vec![1.0, 0.0]),
            2,
            crate::DEFAULT_MIN_DENSE_SIMILARITY,
        );

        assert_eq!(hits.len(), 2);
        assert!(hits.iter().any(|hit| hit.chunk.text == lexical.chunk.text));
        assert!(hits.iter().any(|hit| hit.chunk.text == dense.chunk.text));
    }

    #[test]
    fn equal_fused_scores_use_chunk_id_as_a_stable_tiebreaker() {
        let first = record("first", vec![1.0, 0.0]);
        let second = record("second", vec![0.0, 1.0]);
        let records = [&first, &second];

        let hits = rank(&records, "missing", &Embedding(vec![0.0, 0.0]), 2, 0.0);

        let mut expected = [first.chunk.id, second.chunk.id];
        expected.sort_by_key(|id| id.0);
        assert_eq!(hits[0].chunk.id, expected[0]);
        assert_eq!(hits[1].chunk.id, expected[1]);
    }

    #[test]
    fn equal_multi_term_bm25_scores_are_stable_and_use_chunk_id_tiebreaker() {
        let first = record("alpha beta beta", vec![1.0, 0.0]);
        let second = record("beta alpha beta", vec![0.0, 1.0]);
        let records = [&first, &second];

        let ranked = bm25(&records, "beta alpha", 2);

        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].1, ranked[1].1);
        assert!(ranked[0].0.chunk.id.0 < ranked[1].0.chunk.id.0);
    }

    #[test]
    fn lexical_ranking_matches_heading_context_but_returns_source_text() {
        let mut contextual = record("installation details", vec![0.0, 1.0]);
        contextual.chunk.heading_path = vec!["Operator Guide".into(), "Needleshard".into()];
        let records = [&contextual];

        let ranked = bm25(&records, "needleshard", 1);

        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].0.chunk.text, "installation details");
        assert_eq!(
            ranked[0].0.chunk.retrieval_text(),
            "Operator Guide > Needleshard\n\ninstallation details"
        );
    }
}
