use std::collections::{HashMap, HashSet};

use super::{SelectionPolicy, candidate::RankedCandidate};
use crate::chronicle::indexer::db::SearchResult;

pub(super) fn apply_selection_constraints(
    candidates: &mut [RankedCandidate],
    result_limit: usize,
    policy: SelectionPolicy,
) -> Vec<SearchResult> {
    let mut accepted = Vec::new();
    let mut exact_keys = HashSet::new();
    let mut counts = HashMap::new();
    for candidate in candidates.iter_mut().filter(|candidate| candidate.eligible) {
        let decision = if accepted.len() >= result_limit {
            "result_limit"
        } else if !exact_keys.insert(canonical_text(&candidate.result.text)) {
            "exact_duplicate"
        } else if is_near_duplicate(
            &candidate.result,
            &accepted,
            policy.near_duplicate_threshold,
        ) {
            "near_duplicate"
        } else if counts
            .get(&candidate.result.document_path)
            .copied()
            .unwrap_or(0)
            >= policy.max_chunks_per_document
        {
            "document_cap"
        } else {
            "selected"
        };
        candidate.decision = decision;
        if decision == "selected" {
            *counts
                .entry(candidate.result.document_path.clone())
                .or_insert(0) += 1;
            accepted.push(candidate.result.clone());
        }
    }
    accepted
}

pub(super) fn canonical_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn is_near_duplicate(
    candidate: &SearchResult,
    accepted: &[SearchResult],
    threshold: f32,
) -> bool {
    let candidate_shingles = shingles(&candidate.text);
    if candidate_shingles.is_empty() {
        return false;
    }
    accepted.iter().any(|result| {
        if are_overlapping_neighbors(candidate, result) {
            return false;
        }
        let accepted_shingles = shingles(&result.text);
        let intersection = candidate_shingles.intersection(&accepted_shingles).count();
        let union = candidate_shingles.union(&accepted_shingles).count();
        #[allow(clippy::cast_precision_loss)]
        let overlap = intersection as f32 / union as f32;
        overlap >= threshold
    })
}

/// Keep intentionally overlapping neighbors available so retrieval can return context spanning a
/// chunk boundary; exact duplicates are still removed before this check. The overlap marker is
/// stored with the later chunk because it owns the repeated text.
pub(super) fn are_overlapping_neighbors(left: &SearchResult, right: &SearchResult) -> bool {
    if left.document_path != right.document_path
        || left.chunk_index.abs_diff(right.chunk_index) != 1
    {
        return false;
    }
    if left.chunk_index > right.chunk_index {
        left.overlaps_previous
    } else {
        right.overlaps_previous
    }
}

pub(super) fn shingles(text: &str) -> HashSet<String> {
    let words = text
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    if words.len() < 2 {
        return HashSet::new();
    }
    words
        .windows(2)
        .map(|window| format!("{} {}", window[0], window[1]))
        .collect()
}
