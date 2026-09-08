use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};

use anyhow::{Context, Result, anyhow};
use candle_core::Device;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, instrument};

use super::{
    db::repository::{AccessScope, IndexerDb, SearchResult},
    embedder::Embedder,
};

#[async_trait::async_trait]
pub trait RetrieverApi: Send + Sync {
    async fn search(
        &self,
        query: &str,
        settings: SearchSettings,
        access: AccessScope,
    ) -> Result<RetrievalOutcome>;
    async fn load_embedder(&self) -> Result<()>;
    fn unload_embedder(&self) -> Result<()>;
}

#[derive(Debug)]
pub enum RetrievalOutcome {
    Results(Vec<SearchResult>),
    BadQuestion,
    CorpusEmpty,
    NoResultMeetsThreshold,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchSettings {
    pub limit: usize,
    pub candidate_limit: usize,
    pub distance_threshold: f32,
    pub near_duplicate_threshold: f32,
    pub max_chunks_per_document: usize,
}

impl SearchSettings {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.limit > 0 && self.candidate_limit >= self.limit && self.candidate_limit <= 1000,
            "Invalid retrieval limits"
        );
        anyhow::ensure!(
            self.distance_threshold.is_finite() && self.distance_threshold >= 0.0,
            "Invalid distance threshold"
        );
        anyhow::ensure!(
            (0.0..=1.0).contains(&self.near_duplicate_threshold),
            "Invalid duplicate threshold"
        );
        anyhow::ensure!(
            self.max_chunks_per_document > 0,
            "Document cap must be positive"
        );
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct CandidateDiagnostic {
    pub document: String,
    pub chunk_index: i64,
    pub vector_rank: Option<usize>,
    pub vector_distance: Option<f32>,
    pub vector_passed_threshold: bool,
    pub lexical_rank: Option<usize>,
    pub fused_rank: Option<usize>,
    pub rrf_score: f64,
    pub decision: &'static str,
}

#[derive(Debug, Serialize)]
pub struct RetrievalDiagnostics {
    pub settings: SearchSettings,
    pub candidates: Vec<CandidateDiagnostic>,
}

/// The production selection pipeline, also used by offline evaluation. Diagnostics
/// contain identities and scores only and are never part of `SearchResult` or prompts.
pub fn select_with_diagnostics(
    vector: Vec<SearchResult>,
    lexical: Vec<SearchResult>,
    settings: SearchSettings,
) -> (Vec<SearchResult>, RetrievalDiagnostics) {
    let mut diagnostics = std::collections::BTreeMap::new();
    for (is_vector, ranking) in [(true, &vector), (false, &lexical)] {
        for (index, result) in ranking.iter().enumerate() {
            let record = diagnostics
                .entry((result.document_path.clone(), result.chunk_index))
                .or_insert_with(|| CandidateDiagnostic {
                    document: result.document_path.clone(),
                    chunk_index: result.chunk_index,
                    vector_rank: None,
                    vector_distance: None,
                    vector_passed_threshold: false,
                    lexical_rank: None,
                    fused_rank: None,
                    rrf_score: 0.0,
                    decision: "vector_threshold",
                });
            if is_vector {
                record.vector_rank = Some(index + 1);
                record.vector_distance = result.distance.is_finite().then_some(result.distance);
                record.vector_passed_threshold = result.distance <= settings.distance_threshold;
            } else {
                record.lexical_rank = Some(index + 1);
            }
        }
    }
    let vector = vector
        .into_iter()
        .filter(|r| r.distance <= settings.distance_threshold)
        .collect::<Vec<_>>();
    for ranking in [&vector, &lexical] {
        for (index, result) in ranking.iter().enumerate() {
            if let Some(record) =
                diagnostics.get_mut(&(result.document_path.clone(), result.chunk_index))
            {
                #[allow(clippy::cast_precision_loss)]
                {
                    record.rrf_score += 1.0 / (60.0 + (index + 1) as f64);
                }
            }
        }
    }
    let fused = reciprocal_rank_fusion(vector, lexical);
    let mut accepted = Vec::new();
    let mut exact_keys = HashSet::new();
    let mut counts = HashMap::new();
    for (index, candidate) in fused.into_iter().enumerate() {
        let decision = if accepted.len() >= settings.limit {
            "result_limit"
        } else if !exact_keys.insert(canonical_text(&candidate.text)) {
            "exact_duplicate"
        } else if is_near_duplicate(&candidate, &accepted, settings.near_duplicate_threshold) {
            "near_duplicate"
        } else if counts.get(&candidate.document_path).copied().unwrap_or(0)
            >= settings.max_chunks_per_document
        {
            "document_cap"
        } else {
            "selected"
        };
        if let Some(record) =
            diagnostics.get_mut(&(candidate.document_path.clone(), candidate.chunk_index))
        {
            record.fused_rank = Some(index + 1);
            record.decision = decision;
        }
        if decision == "selected" {
            *counts.entry(candidate.document_path.clone()).or_insert(0) += 1;
            accepted.push(candidate);
        }
    }
    (
        accepted,
        RetrievalDiagnostics {
            settings,
            candidates: diagnostics.into_values().collect(),
        },
    )
}

pub struct Retriever {
    db: IndexerDb,
    embedder: Mutex<Option<Embedder>>,
}

impl Retriever {
    pub fn new(db: IndexerDb) -> Self {
        Self {
            db,
            embedder: Mutex::new(None),
        }
    }

    #[instrument(skip(self))]
    pub async fn load_embedder(&self) -> Result<()> {
        let embedder = tokio::task::spawn_blocking(|| Embedder::load(Device::Cpu))
            .await
            .context("CPU embedder loading task failed")??;

        let mut slot = self
            .embedder
            .lock()
            .map_err(|_| anyhow!("Retriever embedder state is poisoned"))?;

        if slot.is_some() {
            debug!("Retriever embedder already loaded");
            return Ok(());
        }

        *slot = Some(embedder);
        info!("Retriever embedder loaded");
        Ok(())
    }

    pub fn unload_embedder(&self) -> Result<()> {
        let mut slot = self
            .embedder
            .lock()
            .map_err(|_| anyhow!("Retriever embedder state is poisoned"))?;
        slot.take();
        info!("Retriever embedder unloaded");
        Ok(())
    }

    #[instrument(skip(self, query), fields(query_len = query.len(), limit, candidate_limit, distance_threshold, near_duplicate_threshold, max_chunks_per_document))]
    pub async fn search(
        &self,
        query: &str,
        settings: SearchSettings,
        access: AccessScope,
    ) -> Result<RetrievalOutcome> {
        let query = query.trim();

        if query.is_empty() {
            return Ok(RetrievalOutcome::BadQuestion);
        }

        if !self
            .db
            .has_chunks()
            .await
            .context("Failed to inspect Chronicle corpus")?
        {
            return Ok(RetrievalOutcome::CorpusEmpty);
        }

        let embedding = {
            let slot = self
                .embedder
                .lock()
                .map_err(|_| anyhow!("Retriever embedder state is poisoned"))?;
            let embedder = slot.as_ref().ok_or_else(|| {
                anyhow!("Chronicle retriever is not ready; run /chronicle start first")
            })?;

            embedder
                .embed(query)
                .with_context(|| "Failed to embed search query")?
        };

        let (vector, lexical) = tokio::try_join!(
            self.db
                .search_similar_for(&embedding, settings.candidate_limit, access),
            self.db.search_lexical_for(query, settings.candidate_limit, access),
        )?;
        let (results, diagnostics) = select_with_diagnostics(vector, lexical, settings);
        debug!(?diagnostics, "Chronicle retrieval diagnostics");
        if results.is_empty() {
            Ok(RetrievalOutcome::NoResultMeetsThreshold)
        } else {
            Ok(RetrievalOutcome::Results(results))
        }
    }
}

#[async_trait::async_trait]
impl RetrieverApi for Retriever {
    async fn search(
        &self,
        query: &str,
        settings: SearchSettings,
        access: AccessScope,
    ) -> Result<RetrievalOutcome> {
        self.search(query, settings, access).await
    }
    async fn load_embedder(&self) -> Result<()> {
        self.load_embedder().await
    }
    fn unload_embedder(&self) -> Result<()> {
        self.unload_embedder()
    }
}

/// Equal-weight RRF; identities are chunks, never raw similarity scores.
fn reciprocal_rank_fusion(
    vector: Vec<SearchResult>,
    lexical: Vec<SearchResult>,
) -> Vec<SearchResult> {
    let mut candidates: HashMap<(String, i64), (f64, SearchResult)> = HashMap::new();
    for ranking in [vector, lexical] {
        for (rank, result) in ranking.into_iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let score = 1.0 / (60.0 + (rank + 1) as f64);
            let entry = candidates
                .entry((result.document_path.clone(), result.chunk_index))
                .or_insert((0.0, result));
            entry.0 += score;
        }
    }
    let mut candidates = candidates.into_iter().collect::<Vec<_>>();
    candidates.sort_by(|a, b| b.1.0.total_cmp(&a.1.0).then_with(|| a.0.cmp(&b.0)));
    candidates
        .into_iter()
        .map(|(_, (_, result))| result)
        .collect()
}

#[cfg(test)]
fn deduplicate_and_diversify(
    candidates: Vec<SearchResult>,
    limit: usize,
    near_duplicate_threshold: f32,
    max_chunks_per_document: usize,
) -> (Vec<SearchResult>, usize, usize, usize) {
    let (results, diagnostics) = select_with_diagnostics(
        Vec::new(),
        candidates,
        SearchSettings {
            limit,
            candidate_limit: 1000,
            distance_threshold: 0.8,
            near_duplicate_threshold,
            max_chunks_per_document,
        },
    );
    let count = |reason| {
        diagnostics
            .candidates
            .iter()
            .filter(|c| c.decision == reason)
            .count()
    };
    (
        results,
        count("exact_duplicate"),
        count("near_duplicate"),
        count("document_cap"),
    )
}

fn canonical_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_near_duplicate(candidate: &SearchResult, accepted: &[SearchResult], threshold: f32) -> bool {
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
fn are_overlapping_neighbors(left: &SearchResult, right: &SearchResult) -> bool {
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

fn shingles(text: &str) -> HashSet<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn diagnostics_explain_each_selection_decision_without_passage_text() -> Result<()> {
        let mut rejected = result("threshold", 0, "private body text", false);
        rejected.distance = 2.0;
        let lexical = vec![
            result("a", 0, "one two three four five", false),
            result("b", 0, "one two three four five", false),
            result("c", 0, "one two three four five extra", false),
            result("a", 1, "entirely different words", false),
            result("d", 0, "another distinct passage", false),
            result("e", 0, "beyond result budget", false),
        ];
        let (selected, report) = select_with_diagnostics(
            vec![rejected],
            lexical,
            SearchSettings {
                limit: 2,
                candidate_limit: 10,
                distance_threshold: 0.8,
                near_duplicate_threshold: 0.75,
                max_chunks_per_document: 1,
            },
        );
        assert_eq!(selected.len(), 2);
        let reasons = report
            .candidates
            .iter()
            .map(|c| c.decision)
            .collect::<HashSet<_>>();
        for reason in [
            "selected",
            "vector_threshold",
            "exact_duplicate",
            "near_duplicate",
            "document_cap",
            "result_limit",
        ] {
            assert!(reasons.contains(reason), "Missing {reason}");
        }
        assert!(!serde_json::to_string(&report)?.contains("private body text"));
        Ok(())
    }

    #[test]
    fn diagnostics_keep_threshold_rejected_vector_when_lexical_matches() {
        let mut candidate = result("a", 0, "passage", false);
        candidate.distance = 2.0;
        let (selected, report) = select_with_diagnostics(
            vec![candidate.clone()],
            vec![candidate],
            SearchSettings {
                limit: 1,
                candidate_limit: 1,
                distance_threshold: 0.8,
                near_duplicate_threshold: 0.85,
                max_chunks_per_document: 1,
            },
        );
        assert_eq!(selected.len(), 1);
        let candidate = &report.candidates[0];
        assert!(!candidate.vector_passed_threshold);
        assert_eq!(candidate.vector_rank, Some(1));
        assert_eq!(candidate.lexical_rank, Some(1));
        assert_eq!(candidate.decision, "selected");
        assert!((candidate.rrf_score - 1.0 / 61.0).abs() < 1e-10);
    }

    #[test]
    fn fusion_rewards_agreement_and_preserves_lexical_only_hits() {
        let vector = vec![
            result("vector", 0, "semantic", false),
            result("both", 0, "shared", false),
        ];
        let mut lexical_only = result("lexical", 0, "exact name", false);
        lexical_only.distance = f32::INFINITY;
        let lexical = vec![lexical_only, result("both", 0, "shared", false)];
        let fused = reciprocal_rank_fusion(vector, lexical);
        assert_eq!(fused.len(), 3);
        assert_eq!(fused[0].document_path, "both");
        assert!(fused.iter().any(|r| r.document_path == "lexical"));
        assert!(reciprocal_rank_fusion(Vec::new(), Vec::new()).is_empty());
    }

    #[test]
    fn preserves_similar_adjacent_chunks_from_the_same_document() {
        let candidates = vec![
            result("notes.md", 0, "alpha beta gamma delta", false),
            result("notes.md", 1, "beta gamma delta epsilon", true),
        ];

        let (accepted, exact_duplicates, near_duplicates, document_cap) =
            deduplicate_and_diversify(candidates, 2, 0.5, 2);

        assert_eq!(accepted.len(), 2);
        assert_eq!(exact_duplicates, 0);
        assert_eq!(near_duplicates, 0);
        assert_eq!(document_cap, 0);
    }

    #[test]
    fn preserves_overlapping_neighbors_regardless_of_search_order() {
        let candidates = vec![
            result("notes.md", 1, "beta gamma delta epsilon", true),
            result("notes.md", 0, "alpha beta gamma delta", false),
        ];

        let (accepted, exact_duplicates, near_duplicates, document_cap) =
            deduplicate_and_diversify(candidates, 2, 0.5, 2);

        assert_eq!(accepted.len(), 2);
        assert_eq!(exact_duplicates, 0);
        assert_eq!(near_duplicates, 0);
        assert_eq!(document_cap, 0);
    }

    #[test]
    fn filters_similar_chunks_without_a_persisted_overlap() {
        let candidates = vec![
            result("notes.md", 0, "alpha beta gamma delta", false),
            result("notes.md", 1, "beta gamma delta epsilon", false),
        ];

        let (accepted, exact_duplicates, near_duplicates, document_cap) =
            deduplicate_and_diversify(candidates, 2, 0.5, 2);

        assert_eq!(accepted.len(), 1);
        assert_eq!(exact_duplicates, 0);
        assert_eq!(near_duplicates, 1);
        assert_eq!(document_cap, 0);
    }

    #[test]
    fn canonical_text_collapses_whitespace_without_changing_case() {
        assert_eq!(canonical_text("  Alpha\n beta\t"), "Alpha beta");
        assert_ne!(canonical_text("Alpha"), canonical_text("alpha"));
    }

    #[test]
    fn shingles_are_case_insensitive_bigrams() {
        let actual = shingles("Alpha BETA gamma");
        assert_eq!(actual.len(), 2);
        assert!(actual.contains("alpha beta"));
        assert!(actual.contains("beta gamma"));
        assert!(shingles("single").is_empty());
        assert!(shingles("").is_empty());
    }

    #[test]
    fn removes_exact_duplicates_after_whitespace_normalisation() {
        let candidates = vec![
            result("a.md", 0, "alpha beta", false),
            result("b.md", 0, " alpha   beta ", false),
        ];
        let (accepted, exact, near, cap) = deduplicate_and_diversify(candidates, 10, 1.0, 10);
        assert_eq!(accepted.len(), 1);
        assert_eq!((exact, near, cap), (1, 0, 0));
    }

    #[test]
    fn exact_duplicate_detection_is_case_sensitive() {
        let candidates = vec![
            result("a.md", 0, "Alpha beta", false),
            result("b.md", 0, "alpha beta", false),
        ];
        let (accepted, exact, _, _) = deduplicate_and_diversify(candidates, 10, 1.1, 10);
        assert_eq!(accepted.len(), 2);
        assert_eq!(exact, 0);
    }

    #[test]
    fn applies_per_document_cap_and_preserves_ranked_order() {
        let candidates = vec![
            result("a.md", 0, "one alpha", false),
            result("a.md", 1, "two beta", false),
            result("b.md", 0, "three gamma", false),
        ];
        let (accepted, exact, near, cap) = deduplicate_and_diversify(candidates, 10, 1.1, 1);
        assert_eq!(accepted.len(), 2);
        assert_eq!(accepted[0].text, "one alpha");
        assert_eq!(accepted[1].text, "three gamma");
        assert_eq!((exact, near, cap), (0, 0, 1));
    }

    #[test]
    fn respects_zero_and_finite_result_limits() {
        let candidates = vec![
            result("a", 0, "one alpha", false),
            result("b", 0, "two beta", false),
        ];
        assert!(
            deduplicate_and_diversify(candidates.clone(), 0, 1.1, 10)
                .0
                .is_empty()
        );
        let accepted = deduplicate_and_diversify(candidates, 1, 1.1, 10).0;
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].document_path, "a");
    }

    #[test]
    fn identifies_only_marked_adjacent_chunks_as_overlap_neighbors() {
        assert!(are_overlapping_neighbors(
            &result("a", 0, "a b", false),
            &result("a", 1, "b c", true)
        ));
        assert!(!are_overlapping_neighbors(
            &result("a", 0, "a b", false),
            &result("a", 1, "b c", false)
        ));
        assert!(!are_overlapping_neighbors(
            &result("a", 0, "a b", false),
            &result("a", 2, "b c", true)
        ));
        assert!(!are_overlapping_neighbors(
            &result("a", 0, "a b", false),
            &result("b", 1, "b c", true)
        ));
    }

    #[test]
    fn single_word_candidates_are_not_near_duplicates() {
        let accepted = vec![result("a", 0, "word", false)];
        assert!(!is_near_duplicate(
            &result("b", 0, "word", false),
            &accepted,
            0.0
        ));
    }
}
