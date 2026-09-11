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
    fn lexical_match_survives_a_rejected_vector_candidate() {
        let mut candidate = result("a", 0, "passage", false);
        candidate.distance = 2.0;
        let (selected, report) = select_with_diagnostics(
            vec![candidate.clone()],
            vec![candidate],
            settings(1, 0.8, 0.85, 1, 0.0),
        );

        assert_eq!(selected.len(), 1);
        let candidate = &report.candidates[0];
        assert!(!candidate.vector_passed_threshold);
        assert_eq!(candidate.vector_rank, Some(1));
        assert_eq!(candidate.lexical_rank, Some(1));
        assert_eq!(candidate.decision, "selected");
    }

    #[test]
    fn fusion_rewards_agreement_and_keeps_lexical_only_hits() {
        let vector = vec![
            result("vector", 0, "semantic", false),
            result("both", 0, "shared", false),
        ];
        let lexical = vec![
            result("lexical", 0, "exact name", false),
            result("both", 0, "shared", false),
        ];

        let (selected, _) = select_with_diagnostics(vector, lexical, settings(3, 0.8, 1.1, 1, 0.0));

        assert_eq!(selected[0].document_path, "both");
        assert!(
            selected
                .iter()
                .any(|result| result.document_path == "lexical")
        );
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
    fn zero_pagerank_weight_preserves_retrieval_order() {
        let vector = vec![
            result("semantic", 0, "semantic", false),
            result("central", 0, "central", false),
        ];
        let lexical = vec![
            result("central", 0, "central", false),
            result("semantic", 0, "semantic", false),
        ];
        let pagerank = HashMap::from([(
            "central".to_owned(),
            PageRankSignal {
                score: 0.9,
                rank: 1,
            },
        )]);

        let (without_pagerank, _) = select_with_diagnostics(
            vector.clone(),
            lexical.clone(),
            settings(2, 0.8, 0.85, 1, 0.0),
        );
        let (with_zero_weight, diagnostics) = select_with_diagnostics_and_pagerank(
            vector,
            lexical,
            settings(2, 0.8, 0.85, 1, 0.0),
            &pagerank,
        );

        assert_eq!(
            with_zero_weight
                .iter()
                .map(|result| (&result.document_path, result.chunk_index))
                .collect::<Vec<_>>(),
            without_pagerank
                .iter()
                .map(|result| (&result.document_path, result.chunk_index))
                .collect::<Vec<_>>()
        );
        assert!(
            diagnostics
                .candidates
                .iter()
                .all(|candidate| candidate.pagerank_contribution == 0.0)
        );
    }

    #[test]
    fn relevant_peripheral_note_beats_an_irrelevant_hub() {
        let peripheral = result("peripheral", 0, "specific answer", false);
        let hub = result("hub", 0, "generic index", false);
        let pagerank = HashMap::from([
            (
                "hub".to_owned(),
                PageRankSignal {
                    score: 0.8,
                    rank: 1,
                },
            ),
            (
                "peripheral".to_owned(),
                PageRankSignal {
                    score: 0.01,
                    rank: 100,
                },
            ),
        ]);

        let (selected, _) = select_with_diagnostics_and_pagerank(
            vec![peripheral.clone()],
            vec![peripheral, hub],
            settings(2, 0.8, 0.85, 1, 0.15),
            &pagerank,
        );

        assert_eq!(selected[0].document_path, "peripheral");
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
    fn overlap_neighbors_survive_when_input_order_is_reversed() {
        let candidates = vec![
            result("notes.md", 1, "beta gamma delta epsilon", true),
            result("notes.md", 0, "alpha beta gamma delta", false),
        ];
        let (accepted, _) =
            select_with_diagnostics(Vec::new(), candidates, settings(2, 0.8, 0.5, 2, 0.0));

        assert_eq!(accepted.len(), 2);
    }

    #[test]
    fn similar_unmarked_neighbors_are_deduplicated() {
        let candidates = vec![
            result("notes.md", 0, "alpha beta gamma delta", false),
            result("notes.md", 1, "beta gamma delta epsilon", false),
        ];
        let (accepted, report) =
            select_with_diagnostics(Vec::new(), candidates, settings(2, 0.8, 0.5, 2, 0.0));

        assert_eq!(accepted.len(), 1);
        assert!(
            report
                .candidates
                .iter()
                .any(|candidate| candidate.decision == "near_duplicate")
        );
    }

    #[test]
    fn exact_duplicates_normalise_whitespace_but_preserve_case() {
        let whitespace_variants = vec![
            result("a.md", 0, "alpha beta", false),
            result("b.md", 0, " alpha   beta ", false),
        ];
        let (deduplicated, _) = select_with_diagnostics(
            Vec::new(),
            whitespace_variants,
            settings(10, 0.8, 1.1, 10, 0.0),
        );
        assert_eq!(deduplicated.len(), 1);

        let case_variants = vec![
            result("a.md", 0, "Alpha beta", false),
            result("b.md", 0, "alpha beta", false),
        ];
        let (preserved, _) =
            select_with_diagnostics(Vec::new(), case_variants, settings(10, 0.8, 1.1, 10, 0.0));
        assert_eq!(preserved.len(), 2);
    }

    #[test]
    fn document_cap_preserves_the_highest_ranked_candidate() {
        let candidates = vec![
            result("a.md", 0, "one alpha", false),
            result("a.md", 1, "two beta", false),
            result("b.md", 0, "three gamma", false),
        ];
        let (accepted, report) =
            select_with_diagnostics(Vec::new(), candidates, settings(10, 0.8, 1.1, 1, 0.0));

        assert_eq!(
            accepted
                .iter()
                .map(|result| result.text.as_str())
                .collect::<Vec<_>>(),
            ["one alpha", "three gamma"]
        );
        assert!(
            report
                .candidates
                .iter()
                .any(|candidate| candidate.decision == "document_cap")
        );
    }

    #[test]
    fn result_limit_can_be_zero_or_truncate_ranked_results() {
        let candidates = vec![
            result("a", 0, "one alpha", false),
            result("b", 0, "two beta", false),
        ];
        assert!(
            select_with_diagnostics(
                Vec::new(),
                candidates.clone(),
                settings(0, 0.8, 1.1, 10, 0.0)
            )
            .0
            .is_empty()
        );
        let (accepted, _) =
            select_with_diagnostics(Vec::new(), candidates, settings(1, 0.8, 1.1, 10, 0.0));
        assert_eq!(accepted[0].document_path, "a");
    }

    #[test]
    fn single_word_candidates_are_not_near_duplicates() {
        let candidates = vec![result("a", 0, "Word", false), result("b", 0, "word", false)];
        let (accepted, _) =
            select_with_diagnostics(Vec::new(), candidates, settings(2, 0.8, 0.0, 10, 0.0));
        assert_eq!(accepted.len(), 2);
    }

    #[test]
    fn text_normalisation_and_shingles_remain_stable() {
        assert_eq!(canonical_text("  Alpha\n beta\t"), "Alpha beta");
        assert!(shingles("Alpha BETA gamma").contains("alpha beta"));
    }
}
