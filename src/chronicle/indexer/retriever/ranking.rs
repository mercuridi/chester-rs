use std::collections::{BTreeMap, HashMap};

use super::{
    candidate::RankedCandidate,
    settings::{CandidatePoolPolicy, FusionPolicy},
};
use crate::chronicle::indexer::db::{PageRankSignal, SearchResult};

pub(super) fn build_ranked_candidates(
    vector: Vec<SearchResult>,
    lexical: Vec<SearchResult>,
    pagerank: &HashMap<String, PageRankSignal>,
) -> Vec<RankedCandidate> {
    let mut candidates = BTreeMap::new();
    for (is_vector, ranking) in [(true, vector), (false, lexical)] {
        for (index, result) in ranking.into_iter().enumerate() {
            let candidate = candidates
                .entry((result.document_path.clone(), result.chunk_index))
                .or_insert_with(|| {
                    RankedCandidate::new(
                        result.clone(),
                        pagerank.get(&result.document_path).copied(),
                    )
                });
            if is_vector {
                candidate.vector_rank = Some(index + 1);
                candidate.vector_distance = result.distance.is_finite().then_some(result.distance);
            } else {
                candidate.lexical_rank = Some(index + 1);
            }
        }
    }
    candidates.into_values().collect()
}

pub(super) fn filter_vector_candidates(
    candidates: &mut [RankedCandidate],
    policy: CandidatePoolPolicy,
) {
    for candidate in candidates {
        candidate.vector_passed_threshold = candidate.vector_rank.is_some()
            && candidate.result.distance <= policy.distance_threshold;
        candidate.eligible = candidate.vector_passed_threshold || candidate.lexical_rank.is_some();
    }
}

pub(super) fn score_fused_candidates(candidates: &mut [RankedCandidate], policy: FusionPolicy) {
    for candidate in candidates.iter_mut().filter(|candidate| candidate.eligible) {
        for (rank, weight) in [
            (
                candidate
                    .vector_rank
                    .filter(|_| candidate.vector_passed_threshold),
                policy.vector_rrf_weight,
            ),
            (candidate.lexical_rank, policy.lexical_rrf_weight),
        ] {
            let Some(rank) = rank else { continue };
            #[allow(clippy::cast_precision_loss)]
            let contribution = weight / (policy.rrf_rank_constant + rank as f64);
            candidate.rrf_score += contribution;
        }
        if let Some(rank) = candidate.pagerank_rank.filter(|rank| *rank > 0) {
            #[allow(clippy::cast_precision_loss)]
            let contribution = policy.pagerank_weight / (policy.rrf_rank_constant + rank as f64);
            candidate.pagerank_contribution = contribution;
        }
        candidate.final_fusion_score =
            candidate.rrf_score + candidate.pagerank_contribution + candidate.reranker_contribution;
    }
    candidates.sort_by(|left, right| {
        right
            .eligible
            .cmp(&left.eligible)
            .then_with(|| right.final_fusion_score.total_cmp(&left.final_fusion_score))
            .then_with(|| left.key().cmp(&right.key()))
    });
    for (index, candidate) in candidates
        .iter_mut()
        .filter(|candidate| candidate.eligible)
        .enumerate()
    {
        candidate.final_rank = Some(index + 1);
    }
}
