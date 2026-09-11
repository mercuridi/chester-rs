use std::collections::HashMap;

use super::{
    candidate::RankedCandidate,
    diagnostics::RetrievalDiagnostics,
    ranking::{build_ranked_candidates, filter_vector_candidates, score_fused_candidates},
    selection::apply_selection_constraints,
    settings::SearchSettings,
};
use crate::chronicle::indexer::db::repository::facade::{PageRankSignal, SearchResult};

/// The production selection pipeline, also used by offline evaluation. Diagnostics
/// contain identities and scores only and are never part of `SearchResult` or prompts.
pub fn select_with_diagnostics(
    vector: Vec<SearchResult>,
    lexical: Vec<SearchResult>,
    settings: SearchSettings,
) -> (Vec<SearchResult>, RetrievalDiagnostics) {
    select_with_diagnostics_and_pagerank(vector, lexical, settings, &HashMap::new())
}

pub fn select_with_diagnostics_and_pagerank(
    vector: Vec<SearchResult>,
    lexical: Vec<SearchResult>,
    settings: SearchSettings,
    pagerank: &HashMap<String, PageRankSignal>,
) -> (Vec<SearchResult>, RetrievalDiagnostics) {
    let mut candidates = build_ranked_candidates(vector, lexical, pagerank);
    filter_vector_candidates(&mut candidates, settings.candidate_pool);
    score_fused_candidates(&mut candidates, settings.fusion);
    let accepted =
        apply_selection_constraints(&mut candidates, settings.limits.limit, settings.selection);
    (accepted, finalize_diagnostics(settings, candidates))
}

fn finalize_diagnostics(
    settings: SearchSettings,
    mut candidates: Vec<RankedCandidate>,
) -> RetrievalDiagnostics {
    candidates.sort_by_key(RankedCandidate::key);
    RetrievalDiagnostics {
        settings,
        candidates: candidates
            .into_iter()
            .map(RankedCandidate::diagnostic)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chronicle::indexer::retriever::{
        selection::{canonical_text, shingles},
        settings::{CandidatePoolPolicy, FusionPolicy, RetrievalLimits, SelectionPolicy},
    };

    fn result(
        document_path: &str,
        chunk_index: i64,
        text: &str,
        overlaps_previous: bool,
    ) -> SearchResult {
        SearchResult {
            document_path: document_path.to_owned(),
            chunk_index,
            heading: None,
            text: text.to_owned(),
            overlaps_previous,
            distance: 0.0,
        }
    }

    fn settings(
        limit: usize,
        threshold: f32,
        duplicate_threshold: f32,
        cap: usize,
        pagerank_weight: f64,
    ) -> SearchSettings {
        SearchSettings {
            limits: RetrievalLimits {
                limit,
                candidate_limit: 10,
            },
            candidate_pool: CandidatePoolPolicy {
                distance_threshold: threshold,
            },
            fusion: FusionPolicy {
                vector_rrf_weight: 1.0,
                lexical_rrf_weight: 1.0,
                pagerank_weight,
                rrf_rank_constant: 60.0,
            },
            selection: SelectionPolicy {
                near_duplicate_threshold: duplicate_threshold,
                max_chunks_per_document: cap,
            },
        }
    }

    #[test]
    fn diagnostics_explain_selection_without_passage_text() {
        let mut rejected = result("threshold", 0, "private body text", false);
        rejected.distance = 2.0;
        let lexical = vec![
            result("a", 0, "one two three four five", false),
            result("b", 0, "one two three four five", false),
            result("c", 0, "one two three four five extra", false),
            result("a", 1, "entirely different words", false),
            result("d", 0, "another distinct passage", false),
        ];
        let (selected, report) =
            select_with_diagnostics(vec![rejected], lexical, settings(2, 0.8, 0.75, 1, 0.0));
        assert_eq!(selected.len(), 2);
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("private body text")
        );
        for reason in [
            "selected",
            "vector_threshold",
            "exact_duplicate",
            "near_duplicate",
            "document_cap",
        ] {
            assert!(
                report
                    .candidates
                    .iter()
                    .any(|candidate| candidate.decision == reason)
            );
        }
    }

    #[test]
    fn pagerank_breaks_close_relevance_ties() {
        let lexical = vec![
            result("lexical-first", 0, "first", false),
            result("central", 0, "central", false),
        ];
        let pagerank = HashMap::from([(
            "central".to_owned(),
            PageRankSignal {
                score: 0.5,
                rank: 1,
            },
        )]);
        let (selected, diagnostics) = select_with_diagnostics_and_pagerank(
            Vec::new(),
            lexical,
            settings(2, 0.8, 0.85, 1, 0.15),
            &pagerank,
        );
        assert_eq!(selected[0].document_path, "central");
        assert!(
            diagnostics
                .candidates
                .iter()
                .any(|candidate| candidate.document == "central"
                    && candidate.pagerank_contribution > 0.0)
        );
    }

    #[test]
    fn overlap_neighbors_survive_duplicate_selection() {
        let candidates = vec![
            result("notes.md", 0, "alpha beta gamma delta", false),
            result("notes.md", 1, "beta gamma delta epsilon", true),
        ];
        let (accepted, _) =
            select_with_diagnostics(Vec::new(), candidates, settings(2, 0.8, 0.5, 2, 0.0));
        assert_eq!(accepted.len(), 2);
    }

    #[test]
    fn text_normalisation_and_shingles_remain_stable() {
        assert_eq!(canonical_text("  Alpha\n beta\t"), "Alpha beta");
        assert!(shingles("Alpha BETA gamma").contains("alpha beta"));
    }
}
