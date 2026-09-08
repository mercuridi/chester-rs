//! Deterministic `PageRank` over the persisted Chronicle document graph.

use std::collections::HashMap;

pub const DAMPING_FACTOR: f64 = 0.85;
const CONVERGENCE_TOLERANCE: f64 = 1e-12;
const MAX_ITERATIONS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageRankEntry {
    pub document_id: i64,
    pub score: f64,
    /// One-based rank; ties are broken by ascending document ID.
    pub rank: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PageRankResult {
    pub entries: Vec<PageRankEntry>,
    pub iterations: usize,
}

/// Compute `PageRank` for a node-induced directed graph.
///
/// Duplicate nodes and edges are ignored. Edges outside the supplied node set
/// are ignored, which makes the function suitable for access-scoped graphs.
pub fn compute(nodes: &[i64], edges: &[(i64, i64)]) -> PageRankResult {
    let mut node_ids = nodes.to_vec();
    node_ids.sort_unstable();
    node_ids.dedup();
    if node_ids.is_empty() {
        return PageRankResult {
            entries: Vec::new(),
            iterations: 0,
        };
    }

    let node_indexes = node_ids
        .iter()
        .enumerate()
        .map(|(index, document_id)| (*document_id, index))
        .collect::<HashMap<_, _>>();
    let mut outgoing = vec![Vec::new(); node_ids.len()];
    for &(source, target) in edges {
        let (Some(&source), Some(&target)) = (node_indexes.get(&source), node_indexes.get(&target))
        else {
            continue;
        };
        if !outgoing[source].contains(&target) {
            outgoing[source].push(target);
        }
    }

    let node_count = node_ids.len();
    #[allow(clippy::cast_precision_loss)]
    let node_count_f64 = node_count as f64;
    let mut scores = vec![1.0 / node_count_f64; node_count];
    let teleport = (1.0 - DAMPING_FACTOR) / node_count_f64;
    let mut iterations = 0;

    for iteration in 1..=MAX_ITERATIONS {
        let dangling_score = outgoing
            .iter()
            .enumerate()
            .filter(|(_, targets)| targets.is_empty())
            .map(|(index, _)| scores[index])
            .sum::<f64>();
        let mut next =
            vec![teleport + DAMPING_FACTOR * dangling_score / node_count_f64; node_count];
        for (source, targets) in outgoing.iter().enumerate() {
            if targets.is_empty() {
                continue;
            }
            #[allow(clippy::cast_precision_loss)]
            let contribution = DAMPING_FACTOR * scores[source] / targets.len() as f64;
            for &target in targets {
                next[target] += contribution;
            }
        }
        let delta = scores
            .iter()
            .zip(&next)
            .map(|(previous, current)| (previous - current).abs())
            .sum::<f64>();
        scores = next;
        iterations = iteration;
        if delta <= CONVERGENCE_TOLERANCE {
            break;
        }
    }

    let mut ordered = node_ids
        .into_iter()
        .zip(scores)
        .collect::<Vec<(i64, f64)>>();
    ordered.sort_by(|(left_id, left_score), (right_id, right_score)| {
        right_score
            .total_cmp(left_score)
            .then_with(|| left_id.cmp(right_id))
    });
    let entries = ordered
        .into_iter()
        .enumerate()
        .map(|(index, (document_id, score))| PageRankEntry {
            document_id,
            score,
            rank: i64::try_from(index + 1).unwrap_or(i64::MAX),
        })
        .collect();
    PageRankResult {
        entries,
        iterations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn favors_a_document_linked_by_multiple_notes() {
        let result = compute(&[1, 2, 3], &[(1, 2), (3, 2)]);
        assert_eq!(result.entries[0].document_id, 2);
        assert!(result.entries[0].score > result.entries[1].score);
        let score_sum = result.entries.iter().map(|entry| entry.score).sum::<f64>();
        assert!((score_sum - 1.0).abs() < 1e-10);
    }

    #[test]
    fn dangling_nodes_and_duplicate_edges_are_handled_deterministically() {
        let result = compute(&[3, 1, 2], &[(1, 2), (1, 2), (99, 1)]);
        assert!(result.iterations > 0);
        assert_eq!(result.entries.len(), 3);
        assert!(result.entries.iter().all(|entry| entry.score.is_finite()));

        let ties = compute(&[3, 1, 2], &[]);
        assert_eq!(
            ties.entries
                .iter()
                .map(|entry| entry.document_id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }
}
