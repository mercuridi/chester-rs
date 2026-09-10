use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};

use anyhow::{Context, Result, anyhow};
use candle_core::Device;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, instrument};

use super::{
    db::repository::{AccessScope, IndexerDb, PageRankSignal, SearchResult},
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
    pub limits: RetrievalLimits,
    pub candidate_pool: CandidatePoolPolicy,
    pub fusion: FusionPolicy,
    pub selection: SelectionPolicy,
}

/// Operational limits: candidate retrieval work and final context size.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalLimits {
    pub limit: usize,
    pub candidate_limit: usize,
}

/// Candidate-pool policy: determines which raw retrieval results reach fusion.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CandidatePoolPolicy {
    pub distance_threshold: f32,
}

/// Ranking policy: determines how eligible candidates are fused before reranking and selection.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FusionPolicy {
    #[serde(default = "default_rrf_weight")]
    pub vector_rrf_weight: f64,
    #[serde(default = "default_rrf_weight")]
    pub lexical_rrf_weight: f64,
    #[serde(default = "default_pagerank_weight")]
    pub pagerank_weight: f64,
    #[serde(default = "default_rrf_rank_constant")]
    pub rrf_rank_constant: f64,
}

/// Context-selection policy: applies after fusion (and a future reranking stage).
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionPolicy {
    pub near_duplicate_threshold: f32,
    pub max_chunks_per_document: usize,
}

const fn default_pagerank_weight() -> f64 {
    0.15
}

const fn default_rrf_weight() -> f64 {
    1.0
}

const fn default_rrf_rank_constant() -> f64 {
    60.0
}

impl SearchSettings {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.limits.limit > 0
                && self.limits.candidate_limit >= self.limits.limit
                && self.limits.candidate_limit <= 1000,
            "Invalid retrieval limits"
        );
        anyhow::ensure!(
            self.candidate_pool.distance_threshold.is_finite()
                && self.candidate_pool.distance_threshold >= 0.0,
            "Invalid distance threshold"
        );
        anyhow::ensure!(
            (0.0..=1.0).contains(&self.selection.near_duplicate_threshold),
            "Invalid duplicate threshold"
        );
        anyhow::ensure!(
            self.selection.max_chunks_per_document > 0,
            "Document cap must be positive"
        );
        anyhow::ensure!(
            self.fusion.vector_rrf_weight.is_finite()
                && self.fusion.vector_rrf_weight >= 0.0
                && self.fusion.lexical_rrf_weight.is_finite()
                && self.fusion.lexical_rrf_weight >= 0.0
                && self.fusion.pagerank_weight.is_finite()
                && (0.0..=1.0).contains(&self.fusion.pagerank_weight)
                && self.fusion.rrf_rank_constant.is_finite()
                && self.fusion.rrf_rank_constant >= 0.0,
            "Invalid PageRank weight"
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
    /// Lexical and vector reciprocal-rank-fusion score, before graph prior.
    pub rrf_score: f64,
    pub pagerank_score: Option<f64>,
    pub pagerank_rank: Option<i64>,
    pub pagerank_contribution: f64,
    /// The score used to rank this candidate after all fusion inputs.
    pub final_fusion_score: f64,
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

type CandidateKey = (String, i64);

#[derive(Debug)]
struct RankedCandidate {
    result: SearchResult,
    vector_rank: Option<usize>,
    vector_distance: Option<f32>,
    vector_passed_threshold: bool,
    lexical_rank: Option<usize>,
    rrf_score: f64,
    pagerank_score: Option<f64>,
    pagerank_rank: Option<i64>,
    pagerank_contribution: f64,
    /// Reserved for a future reranking stage between fusion and constraints.
    reranker_contribution: f64,
    final_fusion_score: f64,
    eligible: bool,
    final_rank: Option<usize>,
    decision: &'static str,
}

impl RankedCandidate {
    fn new(result: SearchResult, pagerank: Option<PageRankSignal>) -> Self {
        Self {
            result,
            vector_rank: None,
            vector_distance: None,
            vector_passed_threshold: false,
            lexical_rank: None,
            rrf_score: 0.0,
            pagerank_score: pagerank.map(|signal| signal.score),
            pagerank_rank: pagerank.map(|signal| signal.rank),
            pagerank_contribution: 0.0,
            reranker_contribution: 0.0,
            final_fusion_score: 0.0,
            eligible: false,
            final_rank: None,
            decision: "vector_threshold",
        }
    }

    fn key(&self) -> CandidateKey {
        (self.result.document_path.clone(), self.result.chunk_index)
    }

    fn diagnostic(self) -> CandidateDiagnostic {
        CandidateDiagnostic {
            document: self.result.document_path,
            chunk_index: self.result.chunk_index,
            vector_rank: self.vector_rank,
            vector_distance: self.vector_distance,
            vector_passed_threshold: self.vector_passed_threshold,
            lexical_rank: self.lexical_rank,
            fused_rank: self.final_rank,
            rrf_score: self.rrf_score,
            pagerank_score: self.pagerank_score,
            pagerank_rank: self.pagerank_rank,
            pagerank_contribution: self.pagerank_contribution,
            final_fusion_score: self.final_fusion_score,
            decision: self.decision,
        }
    }
}

fn build_ranked_candidates(
    vector: Vec<SearchResult>,
    lexical: Vec<SearchResult>,
    pagerank: &HashMap<String, PageRankSignal>,
) -> Vec<RankedCandidate> {
    let mut candidates = std::collections::BTreeMap::new();
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

fn filter_vector_candidates(candidates: &mut [RankedCandidate], policy: CandidatePoolPolicy) {
    for candidate in candidates {
        candidate.vector_passed_threshold = candidate.vector_rank.is_some()
            && candidate.result.distance <= policy.distance_threshold;
        candidate.eligible = candidate.vector_passed_threshold || candidate.lexical_rank.is_some();
    }
}

fn score_fused_candidates(candidates: &mut [RankedCandidate], policy: FusionPolicy) {
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
            let Some(rank) = rank else {
                continue;
            };
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

fn apply_selection_constraints(
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
                .search_similar_for(&embedding, settings.limits.candidate_limit, access),
            self.db
                .search_lexical_for(query, settings.limits.candidate_limit, access),
        )?;
        let paths = vector
            .iter()
            .chain(&lexical)
            .map(|result| result.document_path.clone())
            .collect::<Vec<_>>();
        let pagerank = self.db.pagerank_for_paths(paths, access).await?;
        let (results, diagnostics) =
            select_with_diagnostics_and_pagerank(vector, lexical, settings, &pagerank);
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
#[cfg(test)]
fn reciprocal_rank_fusion(
    vector: Vec<SearchResult>,
    lexical: Vec<SearchResult>,
) -> Vec<SearchResult> {
    reciprocal_rank_fusion_with_pagerank(vector, lexical, 0.0, &HashMap::new())
}

#[cfg(test)]
fn reciprocal_rank_fusion_with_pagerank(
    vector: Vec<SearchResult>,
    lexical: Vec<SearchResult>,
    pagerank_weight: f64,
    pagerank: &HashMap<String, PageRankSignal>,
) -> Vec<SearchResult> {
    let mut candidates = build_ranked_candidates(vector, lexical, pagerank);
    for candidate in &mut candidates {
        candidate.vector_passed_threshold = candidate.vector_rank.is_some();
        candidate.eligible = candidate.vector_passed_threshold || candidate.lexical_rank.is_some();
    }
    score_fused_candidates(
        &mut candidates,
        FusionPolicy {
            vector_rrf_weight: 1.0,
            lexical_rrf_weight: 1.0,
            pagerank_weight,
            rrf_rank_constant: 60.0,
        },
    );
    candidates
        .into_iter()
        .filter(|candidate| candidate.eligible)
        .map(|candidate| candidate.result)
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
            limits: RetrievalLimits {
                limit,
                candidate_limit: 1000,
            },
            candidate_pool: CandidatePoolPolicy {
                distance_threshold: 0.8,
            },
            fusion: FusionPolicy {
                vector_rrf_weight: 1.0,
                lexical_rrf_weight: 1.0,
                pagerank_weight: 0.0,
                rrf_rank_constant: 60.0,
            },
            selection: SelectionPolicy {
                near_duplicate_threshold,
                max_chunks_per_document,
            },
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

    fn settings(
        limit: usize,
        candidate_limit: usize,
        distance_threshold: f32,
        near_duplicate_threshold: f32,
        max_chunks_per_document: usize,
        pagerank_weight: f64,
    ) -> SearchSettings {
        SearchSettings {
            limits: RetrievalLimits {
                limit,
                candidate_limit,
            },
            candidate_pool: CandidatePoolPolicy { distance_threshold },
            fusion: FusionPolicy {
                vector_rrf_weight: 1.0,
                lexical_rrf_weight: 1.0,
                pagerank_weight,
                rrf_rank_constant: 60.0,
            },
            selection: SelectionPolicy {
                near_duplicate_threshold,
                max_chunks_per_document,
            },
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
        let (selected, report) =
            select_with_diagnostics(vec![rejected], lexical, settings(2, 10, 0.8, 0.75, 1, 0.0));
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
            settings(1, 1, 0.8, 0.85, 1, 0.0),
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
    #[allow(clippy::expect_used)]
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
        let settings = settings(2, 2, 0.8, 0.85, 1, 0.15);
        let (selected, diagnostics) =
            select_with_diagnostics_and_pagerank(Vec::new(), lexical, settings, &pagerank);
        assert_eq!(selected[0].document_path, "central");
        let central = diagnostics
            .candidates
            .iter()
            .find(|candidate| candidate.document == "central")
            .expect("central diagnostic");
        assert_eq!(central.pagerank_rank, Some(1));
        assert!(central.pagerank_contribution > 0.0);
        assert!(
            (central.final_fusion_score - central.rrf_score - central.pagerank_contribution).abs()
                < 1e-12
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
        let settings = settings(2, 2, 0.8, 0.85, 1, 0.0);
        let pagerank = HashMap::from([(
            "central".to_owned(),
            PageRankSignal {
                score: 0.9,
                rank: 1,
            },
        )]);

        let (without_pagerank, _) =
            select_with_diagnostics(vector.clone(), lexical.clone(), settings);
        let (with_zero_weight, diagnostics) =
            select_with_diagnostics_and_pagerank(vector, lexical, settings, &pagerank);

        assert_eq!(
            with_zero_weight
                .iter()
                .map(|candidate| &candidate.document_path)
                .collect::<Vec<_>>(),
            without_pagerank
                .iter()
                .map(|candidate| &candidate.document_path)
                .collect::<Vec<_>>()
        );
        assert!(diagnostics.candidates.iter().all(|candidate| {
            candidate.pagerank_contribution == 0.0
                && (candidate.final_fusion_score - candidate.rrf_score).abs() < f64::EPSILON
        }));
    }

    #[test]
    #[allow(clippy::expect_used)]
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
        let settings = settings(2, 2, 0.8, 0.85, 1, 0.15);

        let (selected, diagnostics) = select_with_diagnostics_and_pagerank(
            vec![peripheral.clone()],
            vec![peripheral, hub],
            settings,
            &pagerank,
        );

        assert_eq!(selected[0].document_path, "peripheral");
        let peripheral = diagnostics
            .candidates
            .iter()
            .find(|candidate| candidate.document == "peripheral")
            .expect("peripheral diagnostic");
        let hub = diagnostics
            .candidates
            .iter()
            .find(|candidate| candidate.document == "hub")
            .expect("hub diagnostic");
        assert!(hub.pagerank_contribution > peripheral.pagerank_contribution);
        assert!(peripheral.final_fusion_score > hub.final_fusion_score);
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
